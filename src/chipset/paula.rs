// SPDX-License-Identifier: GPL-3.0-or-later

//! Paula-side state: serial UART registers, interrupt enable/request,
//! and four-channel audio DMA + mixer.

use crate::audio::mux::AudioMux;
use crate::audio::{AudioRuntimeStatus, AudioSink, MIX_SAMPLE_RATE};
use crate::drive_sounds::DriveSounds;
use crate::serial::SerialSink;
use std::f32::consts::PI;

/// PAL Paula audio clock. The standard sample-rate-from-period formula
/// is `PAULA_CLOCK_HZ / AUDxPER`, e.g. period 254 -> ~13977 Hz. One
/// Paula audio clock tick equals one Amiga color clock for this
/// emulator's device timeline.
pub const PAULA_CLOCK_HZ: u32 = 3_546_895;
pub const PAL_AUDIO_MIN_PERIOD_CCK: u16 = 123;
pub const NTSC_AUDIO_MIN_PERIOD_CCK: u16 = 124;

/// CIA-A IRQ line (keyboard, timers, parallel handshake).
pub const INT_PORTS: u16 = 1 << 3;
pub const INT_VERTB: u16 = 1 << 5;
/// CIA-B IRQ line (disk drives, serial port DCD/CTS).
pub const INT_EXTER: u16 = 1 << 13;
/// Bit 14 is the master interrupt enable in INTENA. The same bit is
/// also latchable in INTREQ as the undocumented INT14 source.
pub const INT_MASTER: u16 = 1 << 14;
pub const INT_INT14: u16 = 1 << 14;

pub const INT_TBE: u16 = 1 << 0;
pub const INT_DSKBLK: u16 = 1 << 1;
// Named for completeness of the INTREQ bit set; nothing raises it yet.
#[allow(dead_code)]
pub const INT_SOFT: u16 = 1 << 2;
pub const INT_COPER: u16 = 1 << 4;
pub const INT_BLIT: u16 = 1 << 6;

/// Per-channel audio interrupt bits in INTENA/INTREQ. In DMA mode the
/// interrupt fires when the first start-up word arrives (so the CPU can
/// prime the *next* buffer's LC/LEN before this one has even played) and
/// again at the word start after each length-counter rollover; in IRQ
/// (CPU-driven) mode it fires as each AUDxDAT word is taken for output.
pub const INT_AUD0: u16 = 1 << 7;
pub const INT_AUD1: u16 = 1 << 8;
pub const INT_AUD2: u16 = 1 << 9;
pub const INT_AUD3: u16 = 1 << 10;
pub const INT_RBF: u16 = 1 << 11;
pub const INT_DSKSYNC: u16 = 1 << 12;
const INT_AUDX: [u16; 4] = [INT_AUD0, INT_AUD1, INT_AUD2, INT_AUD3];
const INTREQ_MASK: u16 = 0x7FFF;
const SERPER_LONG: u16 = 1 << 15;
const ADKCON_UARTBRK: u16 = 1 << 11;

/// DMACON.DMAEN master enable. Stored on agnus.dmacon; Paula audio
/// gating ANDs this with the per-channel AUDxEN bits 0..3.
pub const DMACON_DMAEN: u16 = 1 << 9;
const LED_FILTER_CUTOFF_HZ: f32 = 4_000.0;

/// HRM audio state-machine states (Paula's three per-channel state bits).
/// 000 idle; 001/101 the two DMA start-up fetches; 010/011 outputting the
/// buffer's high/low byte while the period counter runs.
const AUD_IDLE: u8 = 0b000;
const AUD_DMA_FIRST: u8 = 0b001;
const AUD_DMA_SECOND: u8 = 0b101;
const AUD_OUT_HI: u8 = 0b010;
const AUD_OUT_LO: u8 = 0b011;

/// Short label for the debugger's audio tab.
fn aud_state_name(state: u8) -> &'static str {
    match state {
        AUD_IDLE => "000 idle",
        AUD_DMA_FIRST => "001 start",
        AUD_DMA_SECOND => "101 start",
        AUD_OUT_HI => "010 out-hi",
        AUD_OUT_LO => "011 out-lo",
        _ => "invalid",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDmaRequest {
    pub address: u32,
}

/// Read-only snapshot of one audio channel's live state, for the debugger
/// window's Audio tab. Mirrors the private `AudChannel` fields; there is no
/// serialized state here, so exposing it costs nothing at runtime.
#[derive(Debug, Clone, Copy)]
pub struct AudioChannelDebug {
    /// State-machine state (the HRM state number plus a mnemonic).
    pub state: &'static str,
    /// True while the channel is outputting samples (states 010/011).
    pub playing: bool,
    // CPU-visible latches.
    pub lc: u32,
    pub len: u16,
    pub per: u16,
    pub vol: u8,
    // Live state machine.
    pub ptr: u32,
    pub audlen: u16,
    pub audvol: u8,
    pub percnt: u32,
    pub current: i8,
    /// The next word-start transition raises the buffer-rollover interrupt.
    pub intreq2: bool,
    /// AUDxDR posted, waiting for the line-end transfer to Agnus.
    pub sm_request: bool,
    /// Request latched in Agnus, serviced at the channel's fixed DMA slot.
    pub agnus_request: bool,
}

/// One Paula audio channel: the CPU-visible register latches plus the live
/// HRM state machine. The FSM follows the HRM appendix (and vAmiga's
/// StateMachine.cpp reading of it): AUDxDAT arrivals, the period counter,
/// and DMACON edges drive the transitions; everything else falls out.
#[derive(serde::Serialize, serde::Deserialize)]
struct AudChannel {
    // CPU-visible latches (set via MMIO writes).
    lc: u32,
    len: u16,
    per: u16,
    vol: u8,
    /// Last AUDxDAT value written (CPU peek/debugger only).
    dat_latch: u16,

    // Live HRM state machine.
    state: u8,
    /// Live DMA pointer (Agnus AUDxPT). AUDxDSR resets it to `lc`.
    ptr: u32,
    /// Live length counter; reloaded from `len` at DMA start and rollover.
    audlen: u16,
    /// Live volume; reloaded from `vol` at each output word start.
    audvol: u8,
    /// AUDxDAT holding register: the state machine's input word.
    auddat: u16,
    /// Output buffer word whose bytes feed the DAC.
    buffer: u16,
    /// Color clocks until the period counter expires (states 010/011).
    percnt: u32,
    /// AUDxDR: DMA request posted by the state machine, transferred to
    /// Agnus at the end of the current scanline.
    sm_dr: bool,
    /// Request latched in Agnus; the channel's fixed DMA slot services it
    /// on the next line regardless of the DMACON audio bits.
    agnus_dr: bool,
    /// The length counter rolled over; the next word-start transition
    /// raises the channel interrupt.
    intreq2: bool,
    /// DAC input byte. Held between transitions (Paula holds the DC level;
    /// stopping a channel does not recentre the output).
    current: i8,
}

impl AudChannel {
    fn new() -> Self {
        Self {
            lc: 0,
            len: 0,
            per: 0,
            vol: 0,
            dat_latch: 0,
            state: AUD_IDLE,
            ptr: 0,
            audlen: 0,
            audvol: 0,
            auddat: 0,
            buffer: 0,
            percnt: 0,
            sm_dr: false,
            agnus_dr: false,
            intreq2: false,
            current: 0,
        }
    }

    fn outputting(&self) -> bool {
        matches!(self.state, AUD_OUT_HI | AUD_OUT_LO)
    }
}

fn paula_volume_from_word(word: u16) -> u8 {
    ((word & 0x007F) as u8).min(64)
}

#[cfg(test)]
fn read_chip_word_for_audio_test(chip_ram: &[u8], address: u32) -> u16 {
    if chip_ram.is_empty() {
        return 0;
    }
    let off = (address as usize) % chip_ram.len();
    let hi = chip_ram[off] as u16;
    let lo = chip_ram[(off + 1) % chip_ram.len()] as u16;
    (hi << 8) | lo
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct SerialTxShift {
    word: u16,
    long: bool,
    bit_cck: u32,
    remaining_cck: u32,
    bit_index: u8,
    total_bits: u8,
    break_seen: bool,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct SerialRxShift {
    word: u16,
    long: bool,
    bit_cck: u32,
    remaining_cck: u32,
    bit_index: u8,
    total_bits: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct PotPins {
    pub left_x_released: bool,
    pub left_y_released: bool,
    pub right_x_released: bool,
    pub right_y_released: bool,
    /// External paddle resistance from +5 V to each POT pin, in ohms.
    /// `None` is a disconnected/floating pin. The order is POT0X, POT0Y,
    /// POT1X, POT1Y.
    pub resistance_ohms: [Option<u32>; 4],
}

impl Default for PotPins {
    fn default() -> Self {
        Self {
            left_x_released: true,
            left_y_released: true,
            right_x_released: true,
            right_y_released: true,
            resistance_ohms: [None; 4],
        }
    }
}

/// Largest controller resistance specified by the Amiga Hardware Reference
/// Manual. The recommended 470 kΩ +/- 10% part fits under this 528 kΩ limit.
pub const POT_MAX_RESISTANCE_OHMS: u32 = 528_000;

/// Resistance from +5 V to a POT pin that makes the RC scan latch POTxDAT
/// count == `position`: the exact inverse of `pot_resistance_position`, so
/// an analogue controller's stick/paddle position round-trips through the
/// comparator model unchanged.
pub fn pot_position_resistance_ohms(position: u8) -> u32 {
    u32::from(position) * POT_MAX_RESISTANCE_OHMS / u32::from(u8::MAX)
}

/// The count a POTxDAT byte latches for a pin charging through
/// `resistance_ohms`: threshold time is linear in R (see
/// `pot_threshold_count`), calibrated so the documented maximum lands on
/// the last 8-bit count.
pub fn pot_resistance_position(resistance_ohms: u32) -> u8 {
    let resistance = resistance_ohms.min(POT_MAX_RESISTANCE_OHMS);
    resistance
        .saturating_mul(u32::from(u8::MAX))
        .div_ceil(POT_MAX_RESISTANCE_OHMS) as u8
}

/// POTGOR bits 7..1 are documented as a Paula chip-identification field, but
/// the production 8364R7 fitted across OCS/ECS/AGA machines returns zero (and
/// has no software-selectable revision). Keep the readback explicit rather
/// than accidentally leaking POTGO.START or floating-bus state into it.
const POTGOR_PAULA_ID: u16 = 0x0000;

/// CD audio stream from the CD controller to the host mixer. CD-DA is
/// 44.1 kHz stereo, exactly the mixer rate, so the controller pushes one
/// decoded sector (588 frames) per CD frame and the mixer pops one
/// sample pair per output frame; both sides advance on emulated time, so
/// they stay in step. Bounded so a stalled consumer cannot grow it.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CdAudioRing {
    samples: std::collections::VecDeque<(f32, f32)>,
}

/// 32 sectors (~0.43 s) of buffered CD audio.
const CD_AUDIO_RING_LIMIT: usize = 32 * 588;

/// Length of each debugger oscilloscope ring (host output samples, ~5.8 ms
/// at the 44.1 kHz mixer rate).
const AUDIO_SCOPE_LEN: usize = 256;

impl CdAudioRing {
    /// Decode one 2352-byte CD-DA sector (s16le interleaved stereo) into
    /// the ring. Returns false (dropping the sector) when full.
    pub fn push_sector(&mut self, sector: &[u8]) -> bool {
        if self.samples.len() + sector.len() / 4 > CD_AUDIO_RING_LIMIT {
            return false;
        }
        for frame in sector.chunks_exact(4) {
            let left = i16::from_le_bytes([frame[0], frame[1]]);
            let right = i16::from_le_bytes([frame[2], frame[3]]);
            self.samples
                .push_back((f32::from(left) / 32768.0, f32::from(right) / 32768.0));
        }
        true
    }

    /// Room for at least one more sector?
    pub fn wants_sector(&self) -> bool {
        self.samples.len() + 588 <= CD_AUDIO_RING_LIMIT
    }

    pub fn next_sample(&mut self) -> (f32, f32) {
        self.samples.pop_front().unwrap_or((0.0, 0.0))
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

/// Toccata audio stream from the board to the host mixer. Unlike CD-DA's
/// bursty per-sector delivery, the board's own tick accumulates emulated
/// time the same way `Paula::advance_audio` does (see
/// `Toccata::tick`/`push_mixed_frame`'s fifth tap), so the two sides stay
/// in near-lockstep -- a small fixed capacity is a safety margin, not a
/// buffering requirement.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ToccataAudioRing {
    samples: std::collections::VecDeque<(f32, f32)>,
}

/// A generous safety margin (well beyond what near-lockstep production
/// ever needs) so a stalled consumer cannot grow the ring unbounded.
const TOCCATA_AUDIO_RING_LIMIT: usize = 4096;

impl ToccataAudioRing {
    pub fn push_frame(&mut self, left: f32, right: f32) -> bool {
        if self.samples.len() >= TOCCATA_AUDIO_RING_LIMIT {
            return false;
        }
        self.samples.push_back((left, right));
        true
    }

    pub fn next_sample(&mut self) -> (f32, f32) {
        self.samples.pop_front().unwrap_or((0.0, 0.0))
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

/// MHI board audio stream: the board's own mixer-rate cadence
/// (`Mhi::advance_mixer`, `docs/internals/mhi.md`'s "Determinism and
/// timing") already resamples decoded MPEG PCM onto the mixer grid, so this
/// is a plain per-frame pop like [`ToccataAudioRing`], not a rate
/// conversion -- see that ring's doc comment for why a small fixed capacity
/// is a safety margin here too, not a buffering requirement.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct MhiAudioRing {
    samples: std::collections::VecDeque<(f32, f32)>,
}

/// Mirrors [`TOCCATA_AUDIO_RING_LIMIT`]'s rationale.
const MHI_AUDIO_RING_LIMIT: usize = 4096;

impl MhiAudioRing {
    pub fn push_frame(&mut self, left: f32, right: f32) -> bool {
        if self.samples.len() >= MHI_AUDIO_RING_LIMIT {
            return false;
        }
        self.samples.push_back((left, right));
        true
    }

    pub fn next_sample(&mut self) -> (f32, f32) {
        self.samples.pop_front().unwrap_or((0.0, 0.0))
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

/// Push one sample into a debugger oscilloscope ring, evicting the oldest
/// once the ring is full so it always holds the most recent AUDIO_SCOPE_LEN.
fn scope_push(ring: &mut std::collections::VecDeque<i8>, sample: i8) {
    ring.push_back(sample);
    while ring.len() > AUDIO_SCOPE_LEN {
        ring.pop_front();
    }
}

/// Fold one line-mixed source's stereo frame down to the -128..127 mono
/// level its debugger oscilloscope traces.
fn scope_level(left: f32, right: f32) -> i8 {
    (((left + right) * 0.5).clamp(-1.0, 1.0) * 127.0) as i8
}

fn null_serial_sink() -> Box<dyn SerialSink> {
    Box::new(crate::serial::NullSerialSink)
}

fn null_audio_sink() -> AudioMux {
    AudioMux::new(Box::new(crate::audio::NullSink))
}

/// serde default for the skipped `stereo_separation` field: full width, so a
/// restored state that never stored it plays at hardware panning, not mono.
fn default_stereo_separation() -> f32 {
    1.0
}

/// serde default for `led_filter_guest_on`: the guest's /LED line reads engaged
/// until it drives it otherwise, matching the power-on default.
fn default_true() -> bool {
    true
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Paula {
    pub serper: u16,
    pub intena: u16,
    pub intreq: u16,
    // Host-side sinks, not emulated state: a save state skips them and the
    // loader moves the live sinks across to the restored Paula.
    #[serde(skip, default = "null_serial_sink")]
    pub serial: Box<dyn SerialSink>,
    /// Optional debugger tap for completed transmissions. Host-only: save
    /// states carry it over from the live machine like the serial sink.
    #[serde(skip)]
    pub(crate) serial_observer: Option<crate::serial::SerialObserver>,
    #[serde(skip, default = "null_audio_sink")]
    pub audio: AudioMux,
    pub adkcon: u16,
    pub potgo: u16,

    chans: [AudChannel; 4],
    serial_tx_buffer: Option<u16>,
    serial_tx_shift: Option<SerialTxShift>,
    serial_rx_shift: Option<SerialRxShift>,
    serial_rx_buffer: Option<u16>,
    serial_overrun: bool,
    serial_rx_pin_high: bool,
    serial_rx_sync_0_high: bool,
    serial_rx_sync_1_high: bool,
    serial_tx_pin_high: bool,
    pot_counters: [u8; 4],
    pot_running: bool,
    pot_active: [bool; 4],
    pot_discharge_lines: u8,

    // Test-harness scanline bookkeeping for `tick_audio` (the bus drives
    // the real slot walk and line-end request transfer itself).
    #[cfg(test)]
    #[serde(skip)]
    test_last_dmacon: u16,
    #[cfg(test)]
    #[serde(skip)]
    test_line_cck: u32,

    // Mixer host-sample accumulator in units of
    // color-clocks * MIX_SAMPLE_RATE. One output frame is due each
    // time this reaches PAULA_CLOCK_HZ.
    host_sample_acc: u64,

    /// Effective filter state (what the mix actually applies), resolved from
    /// the mode and the guest's /LED line.
    led_filter_enabled: bool,
    /// User override: `Auto` follows the guest, `On`/`Off` force it. A host
    /// preference, skipped and carried across state loads from config like
    /// mono_output.
    #[serde(skip)]
    led_filter_mode: crate::config::AudioFilterMode,
    /// The guest's request via CIA-A /LED (bit 1 low = engaged); what `Auto`
    /// follows. Machine state, so it rides the save state.
    #[serde(default = "default_true")]
    led_filter_guest_on: bool,
    led_filter: StereoLedFilter,
    output_volume: f32,
    // Host output preference: average L/R into both channels. Not part of the
    // emulated machine state, so it is skipped in save states and re-applied
    // from config (carried over across a state load, see Emulator::load_state).
    #[serde(skip)]
    mono_output: bool,
    // Stereo width, 0.0 (mono) to 1.0 (full hardware panning, the default).
    // Host preference; skipped and carried across state loads like mono_output.
    #[serde(skip, default = "default_stereo_separation")]
    stereo_separation: f32,
    // CD audio samples streamed by the CD controller (CD32 Akiko), mixed
    // into the host output at the shared 44.1 kHz mixer rate.
    cd_audio: CdAudioRing,
    // Toccata board audio, already resampled to the mixer rate by the
    // board's own tick before it reaches this ring -- see
    // `ToccataAudioRing`'s doc comment.
    toccata_audio: ToccataAudioRing,
    // MHI board audio, already resampled to the mixer rate by the board's
    // own tick before it reaches this ring -- see `MhiAudioRing`'s doc
    // comment.
    mhi_audio: MhiAudioRing,
    // Set once the device on the serial port has said it makes no sound
    // here, which is the usual answer: a machine with nothing on its MIDI
    // port then costs one predictable branch a sample and nothing else.
    // Not emulated state -- it is a fact about the host wiring.
    #[serde(skip)]
    synth_silent: bool,
    // Synthesized floppy-drive noises (motor/seek/read), mixed into the
    // host frames after the LED filter: the drive is an acoustic source
    // beside the machine, not part of Paula's filtered audio path.
    drive_sounds: DriveSounds,
    dma_addr_mask: u32,
    // Optional host-side recording tap: when Some, every mixed stereo
    // frame is also appended here (before the master output volume, so
    // recordings stay full scale regardless of the volume slider) for
    // the window's video recorder to drain once per emulated frame.
    capture: Option<Vec<(f32, f32)>>,
    // Developer mute switches (debugger audio tab). Muting a channel or a
    // line-mixed source (CD-DA, the in-process MIDI synth, a Toccata board,
    // an MHI board) silences its contribution to the host output only; the
    // Paula state machine, counters and interrupts keep running exactly as
    // before, so this is not emulated state and never touches a save state.
    #[serde(skip)]
    channel_muted: [bool; 4],
    #[serde(skip)]
    cd_muted: bool,
    #[serde(skip)]
    synth_muted: bool,
    #[serde(skip)]
    toccata_muted: bool,
    #[serde(skip)]
    mhi_muted: bool,
    // Rolling output-level scopes for the debugger's oscilloscope meters:
    // one per Paula channel plus one per line-mixed source (CD-DA, the
    // in-process MIDI synth, a Toccata board, an MHI board). Each holds the
    // most recent AUDIO_SCOPE_LEN host output samples (output level = DAC
    // sample * volume for a channel, mixed stereo level for a source,
    // -128..127). Purely a debug tap, so it is skipped by the save state.
    #[serde(skip)]
    channel_scope: [std::collections::VecDeque<i8>; 4],
    #[serde(skip)]
    cd_scope: std::collections::VecDeque<i8>,
    #[serde(skip)]
    synth_scope: std::collections::VecDeque<i8>,
    #[serde(skip)]
    toccata_scope: std::collections::VecDeque<i8>,
    #[serde(skip)]
    mhi_scope: std::collections::VecDeque<i8>,
}

impl Paula {
    pub fn new(serial: Box<dyn SerialSink>, audio: Box<dyn AudioSink>) -> Self {
        Self {
            serper: 0,
            intena: 0,
            intreq: 0,
            serial,
            serial_observer: None,
            audio: AudioMux::new(audio),
            adkcon: 0,
            potgo: 0,
            chans: [
                AudChannel::new(),
                AudChannel::new(),
                AudChannel::new(),
                AudChannel::new(),
            ],
            serial_tx_buffer: None,
            serial_tx_shift: None,
            serial_rx_shift: None,
            serial_rx_buffer: None,
            serial_overrun: false,
            serial_rx_pin_high: true,
            serial_rx_sync_0_high: true,
            serial_rx_sync_1_high: true,
            serial_tx_pin_high: true,
            pot_counters: [0; 4],
            pot_running: false,
            pot_active: [false; 4],
            pot_discharge_lines: 0,
            #[cfg(test)]
            test_last_dmacon: 0,
            #[cfg(test)]
            test_line_cck: 0,
            host_sample_acc: 0,
            led_filter_enabled: true,
            led_filter_mode: crate::config::AudioFilterMode::Auto,
            led_filter_guest_on: true,
            led_filter: StereoLedFilter::new(),
            output_volume: 1.0,
            mono_output: false,
            stereo_separation: 1.0,
            cd_audio: CdAudioRing::default(),
            toccata_audio: ToccataAudioRing::default(),
            mhi_audio: MhiAudioRing::default(),
            synth_silent: false,
            drive_sounds: DriveSounds::new(),
            dma_addr_mask: 0x001F_FFFF,
            capture: None,
            channel_muted: [false; 4],
            cd_muted: false,
            synth_muted: false,
            toccata_muted: false,
            mhi_muted: false,
            channel_scope: std::array::from_fn(|_| {
                std::collections::VecDeque::with_capacity(AUDIO_SCOPE_LEN + 1)
            }),
            cd_scope: std::collections::VecDeque::with_capacity(AUDIO_SCOPE_LEN + 1),
            synth_scope: std::collections::VecDeque::with_capacity(AUDIO_SCOPE_LEN + 1),
            toccata_scope: std::collections::VecDeque::with_capacity(AUDIO_SCOPE_LEN + 1),
            mhi_scope: std::collections::VecDeque::with_capacity(AUDIO_SCOPE_LEN + 1),
        }
    }

    /// Enable or disable the recording tap. Enabling starts with an
    /// empty buffer; disabling discards anything not yet drained.
    pub fn set_audio_capture_enabled(&mut self, enabled: bool) {
        self.capture = enabled.then(Vec::new);
    }

    /// Drain the mixed stereo frames captured since the last call.
    /// Returns an empty Vec when the tap is disabled.
    pub fn take_captured_audio(&mut self) -> Vec<(f32, f32)> {
        match &mut self.capture {
            Some(buf) => std::mem::take(buf),
            None => Vec::new(),
        }
    }

    /// Enable or disable the bounded host-side serial transmit tap.
    pub fn set_serial_observation_enabled(&mut self, enabled: bool) {
        self.serial_observer = enabled.then(crate::serial::SerialObserver::default);
    }

    /// Drain transmissions completed since the last poll, plus the number of
    /// oldest records evicted because the observer was not drained in time.
    pub fn take_serial_observations(&mut self) -> (Vec<crate::serial::SerialTxObservation>, u64) {
        self.serial_observer
            .as_mut()
            .map(crate::serial::SerialObserver::drain)
            .unwrap_or_default()
    }

    /// Developer mute for one audio channel (debugger audio tab). Toggling
    /// silences the channel's contribution to the host output without
    /// disturbing its state machine, counters or interrupts.
    pub fn toggle_channel_muted(&mut self, ch_idx: usize) {
        if let Some(flag) = self.channel_muted.get_mut(ch_idx) {
            *flag = !*flag;
        }
    }

    pub fn channel_muted(&self, ch_idx: usize) -> bool {
        self.channel_muted.get(ch_idx).copied().unwrap_or(false)
    }

    /// Developer mute for the CD-DA stream (CDTV/CD32).
    pub fn toggle_cd_muted(&mut self) {
        self.cd_muted = !self.cd_muted;
    }

    pub fn cd_muted(&self) -> bool {
        self.cd_muted
    }

    /// Developer mute for the in-process MIDI synth (MT-32/Coppersynth).
    pub fn toggle_synth_muted(&mut self) {
        self.synth_muted = !self.synth_muted;
    }

    pub fn synth_muted(&self) -> bool {
        self.synth_muted
    }

    /// Developer mute for a Toccata board's line-mixed output.
    pub fn toggle_toccata_muted(&mut self) {
        self.toccata_muted = !self.toccata_muted;
    }

    pub fn toccata_muted(&self) -> bool {
        self.toccata_muted
    }

    /// Developer mute for an MHI board's line-mixed output.
    pub fn toggle_mhi_muted(&mut self) {
        self.mhi_muted = !self.mhi_muted;
    }

    pub fn mhi_muted(&self) -> bool {
        self.mhi_muted
    }

    /// Snapshot of a channel's oscilloscope ring (oldest..newest output
    /// levels, up to AUDIO_SCOPE_LEN samples) for the debugger meter.
    pub fn audio_scope_samples(&self, ch_idx: usize) -> Vec<i8> {
        self.channel_scope
            .get(ch_idx)
            .map(|ring| ring.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Snapshot of the CD-DA oscilloscope ring (oldest..newest).
    pub fn cd_scope_samples(&self) -> Vec<i8> {
        self.cd_scope.iter().copied().collect()
    }

    /// Snapshot of the in-process MIDI synth's oscilloscope ring
    /// (oldest..newest).
    pub fn synth_scope_samples(&self) -> Vec<i8> {
        self.synth_scope.iter().copied().collect()
    }

    /// Snapshot of the Toccata board's oscilloscope ring (oldest..newest).
    pub fn toccata_scope_samples(&self) -> Vec<i8> {
        self.toccata_scope.iter().copied().collect()
    }

    /// Snapshot of the MHI board's oscilloscope ring (oldest..newest).
    pub fn mhi_scope_samples(&self) -> Vec<i8> {
        self.mhi_scope.iter().copied().collect()
    }

    pub fn set_dma_addr_mask(&mut self, mask: u32) {
        self.dma_addr_mask = mask | 1;
        let ptr_mask = self.dma_ptr_mask();
        for ch in &mut self.chans {
            ch.lc &= ptr_mask;
            ch.ptr &= ptr_mask;
        }
    }

    #[cfg(test)]
    pub fn set_audio_dma_ptr_for_test(&mut self, ch_idx: usize, ptr: u32) {
        let ptr = ptr & self.dma_ptr_mask();
        if let Some(ch) = self.chans.get_mut(ch_idx) {
            ch.ptr = ptr;
        }
    }

    #[cfg(test)]
    pub fn audio_dma_ptr_for_test(&self, ch_idx: usize) -> Option<u32> {
        self.chans.get(ch_idx).map(|ch| ch.ptr)
    }

    #[cfg(test)]
    pub fn audio_current_sample_for_test(&self, ch_idx: usize) -> Option<i8> {
        self.chans.get(ch_idx).map(|ch| ch.current)
    }

    /// Read-only snapshot of a channel's live state for the debugger.
    pub fn audio_channel_debug(&self, ch_idx: usize) -> Option<AudioChannelDebug> {
        let ch = self.chans.get(ch_idx)?;
        Some(AudioChannelDebug {
            state: aud_state_name(ch.state),
            playing: ch.outputting(),
            lc: ch.lc,
            len: ch.len,
            per: ch.per,
            vol: ch.vol,
            ptr: ch.ptr,
            audlen: ch.audlen,
            audvol: ch.audvol,
            percnt: ch.percnt,
            current: ch.current,
            intreq2: ch.intreq2,
            sm_request: ch.sm_dr,
            agnus_request: ch.agnus_dr,
        })
    }

    pub fn reset_registers(&mut self) {
        self.serper = 0;
        self.intena = 0;
        self.intreq = 0;
        self.adkcon = 0;
        self.potgo = 0;
        self.chans = [
            AudChannel::new(),
            AudChannel::new(),
            AudChannel::new(),
            AudChannel::new(),
        ];
        self.serial_tx_buffer = None;
        self.serial_tx_shift = None;
        self.serial_rx_shift = None;
        self.serial_rx_buffer = None;
        self.serial_overrun = false;
        self.serial_rx_pin_high = true;
        self.serial_rx_sync_0_high = true;
        self.serial_rx_sync_1_high = true;
        self.serial_tx_pin_high = true;
        self.pot_counters = [0; 4];
        self.pot_running = false;
        self.pot_active = [false; 4];
        self.pot_discharge_lines = 0;
        self.host_sample_acc = 0;
        // A reset releases the guest's /LED line; the filter override is a host
        // preference and stays put.
        self.led_filter_guest_on = true;
        self.recompute_led_filter();
        self.led_filter = StereoLedFilter::new();
    }

    /// Record the guest's /LED line (CIA-A PRA bit 1: true = engaged). Followed
    /// only in `Auto` mode.
    pub fn set_led_filter_guest(&mut self, on: bool) {
        self.led_filter_guest_on = on;
        self.recompute_led_filter();
    }

    /// Set the user's filter override (`Auto`/`On`/`Off`).
    pub fn set_led_filter_mode(&mut self, mode: crate::config::AudioFilterMode) {
        self.led_filter_mode = mode;
        self.recompute_led_filter();
    }

    pub fn led_filter_mode(&self) -> crate::config::AudioFilterMode {
        self.led_filter_mode
    }

    fn recompute_led_filter(&mut self) {
        use crate::config::AudioFilterMode;
        self.led_filter_enabled = match self.led_filter_mode {
            AudioFilterMode::On => true,
            AudioFilterMode::Off => false,
            AudioFilterMode::Auto => self.led_filter_guest_on,
        };
    }

    /// Publish the emulated-to-host time mapping to the serial sink.
    pub fn set_serial_time_anchor(&mut self, anchor: crate::serial::SerialTimeAnchor) {
        self.serial.set_time_anchor(anchor);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn led_filter_enabled(&self) -> bool {
        self.led_filter_enabled
    }

    /// The guest's /LED line itself (CIA-A PRA bit 1: true = engaged).
    /// What the machine's power LED shows: the user's filter override
    /// changes the mix, not the pin.
    pub fn led_filter_guest_on(&self) -> bool {
        self.led_filter_guest_on
    }

    pub fn set_output_volume_percent(&mut self, percent: u8) {
        self.output_volume = f32::from(percent.min(100)) / 100.0;
    }

    /// Average the stereo output into both channels (mono) instead of the
    /// hardware's hard left/right panning. Host preference; does not affect the
    /// emulated audio state.
    pub fn set_mono_output(&mut self, mono: bool) {
        self.mono_output = mono;
    }

    pub fn mono_output(&self) -> bool {
        self.mono_output
    }

    /// Stereo width, `0.0` (mono) to `1.0` (full hardware panning). Values are
    /// clamped to that range. Host preference; does not affect emulated state.
    pub fn set_stereo_separation(&mut self, separation: f32) {
        self.stereo_separation = separation.clamp(0.0, 1.0);
    }

    pub fn stereo_separation(&self) -> f32 {
        self.stereo_separation
    }

    pub fn drive_sounds_mut(&mut self) -> &mut DriveSounds {
        &mut self.drive_sounds
    }

    /// Ask the serial sink for audio again. The mixer latches "this device
    /// makes no sound here" so it is not asking once a sample; changing the
    /// device on the port clears that.
    pub fn rearm_synth_audio(&mut self) {
        self.synth_silent = false;
    }

    pub fn cd_audio_mut(&mut self) -> &mut CdAudioRing {
        &mut self.cd_audio
    }

    pub fn toccata_audio_mut(&mut self) -> &mut ToccataAudioRing {
        &mut self.toccata_audio
    }

    pub fn mhi_audio_mut(&mut self) -> &mut MhiAudioRing {
        &mut self.mhi_audio
    }

    /// All three rings together, as disjoint field borrows -- the bus's
    /// generic Zorro-board tick host needs all of them at once
    /// (`DeviceHost::for_slot_with_audio`), which separate `&mut self`
    /// accessor calls cannot express.
    pub fn audio_rings_mut(
        &mut self,
    ) -> (&mut CdAudioRing, &mut ToccataAudioRing, &mut MhiAudioRing) {
        (
            &mut self.cd_audio,
            &mut self.toccata_audio,
            &mut self.mhi_audio,
        )
    }

    pub fn output_volume_percent(&self) -> u8 {
        (self.output_volume * 100.0).round().clamp(0.0, 100.0) as u8
    }

    pub fn live_audio_output_lead_seconds(&self) -> f64 {
        self.audio.live_output_lead_seconds()
    }

    pub fn live_audio_status(&self) -> AudioRuntimeStatus {
        self.audio.runtime_status()
    }

    pub fn set_live_audio_suspended(&mut self, suspended: bool) {
        self.audio.set_live_output_suspended(suspended);
    }

    pub fn reset_live_audio_after_timeline_jump(&mut self) {
        self.audio.reset_live_output_after_timeline_jump();
    }

    /// SERDAT write: bits 7..0 are the data byte; bit 8 is either the
    /// ninth data bit or the first stop bit depending on SERPER. The
    /// model keeps a one-word transmit buffer and a timed shift register.
    pub fn write_serdat(&mut self, val: u16) -> u16 {
        self.serial_tx_buffer = Some(val);
        self.load_serial_shift_if_idle()
    }

    /// SERDATR read: bit 13 = TBE (transmit buffer empty), bit 12 =
    /// TSRE (transmit shift register empty), bit 14 = RBF (receive
    /// buffer full), bit 15 = overrun.
    pub fn read_serdatr(&self) -> u16 {
        let mut v = self.serial_rx_buffer.unwrap_or(0);
        if self.serial_overrun {
            v |= 1 << 15;
        }
        if self.rbf_mirror() {
            v |= 1 << 14;
        }
        if self.serial_tx_buffer.is_none() {
            v |= 1 << 13;
        }
        if self.serial_tx_shift.is_none() {
            v |= 1 << 12;
        }
        if self.serial_rx_sync_1_high {
            v |= 1 << 11;
        }
        v
    }

    /// SERPER write: bits 14..0 are the bit period (in Paula clocks minus
    /// one) and bit 15 selects the long/9-bit word format (`SERPER_LONG`).
    /// Pacing reads `serper` directly at shift time; this also tells the
    /// sink the resulting line rate, for a sink whose protocol text quotes
    /// a baud figure (a modem's CONNECT message).
    pub fn write_serper(&mut self, val: u16) {
        self.serper = val;
        let divisor = u32::from(val & 0x7FFF).saturating_add(1).max(1);
        self.serial.baud_changed(PAULA_CLOCK_HZ / divisor);
    }

    /// INTENA writes use SET/CLR semantics on bit 15.
    pub fn write_intena(&mut self, val: u16) {
        let bits = val & 0x7FFF;
        if val & 0x8000 != 0 {
            self.intena |= bits;
        } else {
            self.intena &= !bits;
        }
    }

    /// INTREQ writes also use SET/CLR semantics. Returns true if the
    /// write asserted a new bit (used by the bus to preempt the slice
    /// so the freshly-set IRQ delivers before agnus piles on VERTB).
    pub fn write_intreq(&mut self, val: u16) -> bool {
        self.write_intreq_with_source_bits(val, 0)
    }

    pub fn write_intreq_with_source_bits(&mut self, val: u16, source_bits: u16) -> bool {
        let bits = val & INTREQ_MASK;
        let source_bits = source_bits & INTREQ_MASK;
        let before = self.intreq;
        if val & 0x8000 != 0 {
            self.intreq |= bits;
        } else {
            self.intreq &= !bits;
            // Clearing RBF releases the receiver for the next word and
            // clears OVRUN, but the receive buffer is a physical latch:
            // its data stays readable in SERDATR until the next word
            // overwrites it. AROS's level-5 dispatcher relies on this --
            // it acks INTREQ BEFORE running the RBF handler, which then
            // reads the still-latched word from SERDATR.
            if bits & INT_RBF != 0 && source_bits & INT_RBF == 0 {
                self.serial_overrun = false;
            }
        }
        self.intreq |= source_bits;
        (self.intreq & !before) != 0
    }

    pub fn latch_interrupt_sources(&mut self, source_bits: u16) -> bool {
        self.write_intreq_with_source_bits(0, source_bits)
    }

    #[cfg(test)]
    pub fn audpen_bits(&self) -> u8 {
        ((self.intreq >> 7) & 0x0F) as u8
    }

    fn rbf_mirror(&self) -> bool {
        self.intreq & INT_RBF != 0
    }

    /// ADKCON: audio modulation control and disk/serial mode bits.
    pub fn write_adkcon(&mut self, val: u16) {
        let bits = val & 0x7FFF;
        if val & 0x8000 != 0 {
            self.adkcon |= bits;
        } else {
            self.adkcon &= !bits;
        }
    }

    fn channel_attached_as_modulator(&self, ch_idx: usize) -> bool {
        let volume_attach = 1u16 << ch_idx;
        let period_attach = 1u16 << (ch_idx + 4);
        self.adkcon & (volume_attach | period_attach) != 0
    }

    pub fn write_potgo(&mut self, val: u16) {
        self.potgo = val & 0xFF01;
        if val & 0x0001 != 0 {
            self.pot_counters = [0; 4];
            self.pot_running = true;
            self.pot_active = [true; 4];
            self.pot_discharge_lines = 0;
        }
    }

    pub fn pot_running(&self) -> bool {
        self.pot_running
    }

    pub fn read_potdat(&self, port: usize) -> u16 {
        match port {
            0 => ((self.pot_counters[1] as u16) << 8) | self.pot_counters[0] as u16,
            _ => ((self.pot_counters[3] as u16) << 8) | self.pot_counters[2] as u16,
        }
    }

    pub fn read_potgor(&self, pins: PotPins) -> u16 {
        let mut v = (self.potgo & 0xFF00) | POTGOR_PAULA_ID;
        for (bit, released) in [
            (8, pins.left_x_released),
            (10, pins.left_y_released),
            (12, pins.right_x_released),
            (14, pins.right_y_released),
        ] {
            let out_bit = bit + 1;
            let mask = 1u16 << bit;
            // The pot pins are open-drain with a weak pull-up: a connected
            // button is a switch to ground. Driving the pin LOW (output enable
            // + data 0) forces it low, but driving it HIGH (output enable +
            // data 1) is only a pull-up that a pressed button still pulls low.
            // With output disabled the pin floats and likewise reads the
            // button. So the button is visible in every mode except a hard low
            // drive -- this is how software reads fire 2/3 by enabling the
            // pull-up (e.g. AmigaTestKit writes POTGO = 0x0f00 << port*4).
            let driven_low = self.potgo & (1u16 << out_bit) != 0 && self.potgo & mask == 0;
            if driven_low || !released {
                v &= !mask;
            } else {
                v |= mask;
            }
        }
        v
    }

    /// True when pot pin `index` (0=POT0X, 1=POT0Y, 2=POT1X, 3=POT1Y) is being
    /// driven HIGH as an output through POTGO -- both its output-enable (OUTxx)
    /// and data (DATxx) bits set. DATxx live at bits 8/10/12/14 with the
    /// matching OUTxx one bit above, the same layout `read_potgor` decodes.
    fn pot_pin_driven_high(potgo: u16, index: usize) -> bool {
        let dat_bit = 8 + 2 * index as u16;
        let out_bit = dat_bit + 1;
        potgo & (1 << dat_bit) != 0 && potgo & (1 << out_bit) != 0
    }

    /// Convert a controller resistance to the scanline on which its RC charge
    /// reaches Paula's comparator threshold. For a fixed capacitor, supply and
    /// threshold, `t = -R*C*ln(1 - Vthreshold/Vcc)`, so threshold time is
    /// linear in R. Calibrate the documented 528 kΩ maximum to the last 8-bit
    /// count; the recommended 470 kΩ part therefore lands at count 227.
    fn pot_threshold_count(resistance_ohms: u32) -> u8 {
        pot_resistance_position(resistance_ohms)
    }

    /// Advance the pot capacitor scan by one horizontal line. Paula holds all
    /// four capacitors discharged for the first 8 PAL or 7 NTSC lines after
    /// START. It then increments an input channel once per H-sync until the
    /// channel's RC charge crosses the comparator threshold, latching that
    /// count. Floating, grounded, or output-low pins never cross and keep
    /// wrapping; output-high pins cross immediately unless an external button
    /// is holding the pin low.
    pub fn tick_pot_hsync(&mut self, pins: PotPins, discharge_lines: u8) {
        if !self.pot_running {
            return;
        }
        let potgo = self.potgo;

        let released = [
            pins.left_x_released,
            pins.left_y_released,
            pins.right_x_released,
            pins.right_y_released,
        ];

        // Output-high is a low-impedance charge path and trips immediately,
        // but the controller button is a switch to ground and overrides it.
        for (i, active) in self.pot_active.iter_mut().enumerate() {
            if *active && released[i] && Self::pot_pin_driven_high(potgo, i) {
                *active = false;
                self.pot_counters[i] = 0;
            }
        }

        if !self.pot_active.iter().any(|active| *active) {
            self.pot_running = false;
            return;
        }

        if self.pot_discharge_lines < discharge_lines {
            self.pot_discharge_lines += 1;
            self.pot_counters = [0; 4];
            return;
        }

        for i in 0..self.pot_counters.len() {
            if !self.pot_active[i] {
                continue;
            }

            let dat_bit = 8 + 2 * i as u16;
            let out_bit = dat_bit + 1;
            let output_enabled = potgo & (1 << out_bit) != 0;
            let output_low = output_enabled && potgo & (1 << dat_bit) == 0;
            let grounded = !released[i];

            // A grounded/button-held or output-low pin remains below the
            // comparator threshold. A disconnected input behaves the same.
            let threshold = if grounded || output_low {
                None
            } else {
                pins.resistance_ohms[i].map(Self::pot_threshold_count)
            };
            if threshold == Some(0) {
                self.pot_active[i] = false;
                continue;
            }

            self.pot_counters[i] = self.pot_counters[i].wrapping_add(1);
            if threshold.is_some_and(|threshold| self.pot_counters[i] >= threshold) {
                self.pot_active[i] = false;
            }
        }
        self.pot_running = self.pot_active.iter().any(|active| *active);
    }

    /// Advance the serial port by `cck` color clocks. `end_cck` is the power-on
    /// color-clock count at the end of this span; a byte that finishes mid-span
    /// is stamped with its emit time from it for a timing-sensitive sink.
    pub fn tick_serial(&mut self, cck: u32, end_cck: u64) -> u16 {
        // The sink's once-per-span heartbeat, ahead of the idle fast path
        // below so a sink with its own elapsed-time timers (an escape
        // guard, a ring cadence) still gets a beat while nothing is
        // shifting.
        self.serial.poll(end_cck);
        // Idle fast path: nothing shifting, nothing queued in either
        // direction. Equivalent to the full path, which would only advance
        // the RX synchronizer (the pin level cannot change while idle).
        if self.serial_tx_shift.is_none()
            && self.serial_tx_buffer.is_none()
            && self.serial_rx_shift.is_none()
            && !self.serial.has_pending_input()
        {
            self.advance_serial_rx_synchronizer(cck);
            return 0;
        }
        self.tick_serial_tx(cck, end_cck) | self.tick_serial_rx(cck)
    }

    fn tick_serial_tx(&mut self, cck: u32, end_cck: u64) -> u16 {
        let mut irq = 0;
        let mut remaining = cck;
        while remaining > 0 {
            if let Some(mut shift) = self.serial_tx_shift.take() {
                let step = remaining.min(shift.remaining_cck);
                remaining -= step;
                shift.remaining_cck -= step;
                shift.break_seen |= self.uart_break_active();
                if shift.remaining_cck > 0 {
                    self.serial_tx_shift = Some(shift);
                    break;
                }

                shift.bit_index += 1;
                if shift.bit_index >= shift.total_bits {
                    if !shift.break_seen && !self.uart_break_active() {
                        // Stop bit done: this many clocks are left of the span.
                        let at_cck = end_cck.saturating_sub(u64::from(remaining));
                        let word = Self::serial_tx_data_word(&shift);
                        if let Some(observer) = self.serial_observer.as_mut() {
                            observer.push(crate::serial::SerialTxObservation {
                                word,
                                long: shift.long,
                                at_cck,
                            });
                        }
                        self.serial.write_word(word, shift.long, at_cck);
                    }
                    self.serial_tx_pin_high = true;
                    irq |= self.load_serial_shift_if_idle();
                } else {
                    shift.remaining_cck = shift.bit_cck;
                    self.serial_tx_pin_high = Self::serial_tx_bit(&shift);
                    self.serial_tx_shift = Some(shift);
                }
            } else {
                irq |= self.load_serial_shift_if_idle();
                if self.serial_tx_shift.is_none() {
                    break;
                }
            }
        }
        irq
    }

    fn tick_serial_rx(&mut self, cck: u32) -> u16 {
        let mut irq = 0;
        let long = self.serial_long();
        self.load_serial_rx_shift_if_idle(long);

        let mut remaining = cck;
        while remaining > 0 {
            let Some(mut shift) = self.serial_rx_shift.take() else {
                self.advance_serial_rx_synchronizer(remaining);
                break;
            };
            let step = remaining.min(shift.remaining_cck);
            remaining -= step;
            shift.remaining_cck -= step;
            self.advance_serial_rx_synchronizer(step);
            if shift.remaining_cck > 0 {
                self.serial_rx_shift = Some(shift);
                break;
            }

            shift.bit_index += 1;
            if shift.bit_index >= shift.total_bits {
                let word = Self::serdatr_receive_word(shift.word, shift.long);
                // Overrun is gated on the RBF interrupt flag (the buffer
                // itself always holds the last received word): a word
                // completing while RBF is still pending is dropped and
                // latches OVRUN. `irq` carries completions from earlier in
                // this same span that the caller has not latched yet.
                if (self.intreq | irq) & INT_RBF != 0 {
                    self.serial_overrun = true;
                } else {
                    self.serial_rx_buffer = Some(word);
                    irq |= INT_RBF;
                }
                self.serial_rx_pin_high = true;
                self.load_serial_rx_shift_if_idle(long);
            } else {
                shift.remaining_cck = shift.bit_cck;
                self.serial_rx_pin_high = Self::serial_rx_bit(&shift);
                self.serial_rx_shift = Some(shift);
            }
        }
        irq
    }

    pub fn next_serial_event_cck(&self) -> Option<u32> {
        let tx = self
            .serial_tx_shift
            .as_ref()
            .map(|shift| shift.remaining_cck.max(1));
        // Host input waiting with the receiver idle is an imminent event
        // too: real Paula starts shifting the moment the start bit hits the
        // pin, so the next span must be short enough to load the shift now.
        // Without this, an idle machine runs a multi-millisecond span in
        // which a whole burst loads AND completes inside one tick_serial
        // call -- the first word buffers, the rest overrun -- before the
        // CPU ever gets to service RBF, dropping bytes real hardware would
        // have delivered.
        let rx = self
            .serial_rx_shift
            .as_ref()
            .map(|shift| shift.remaining_cck.max(1))
            .or_else(|| self.serial.has_pending_input().then_some(1));
        match (tx, rx) {
            (Some(tx), Some(rx)) => Some(tx.min(rx)),
            (Some(tx), None) => Some(tx),
            (None, Some(rx)) => Some(rx),
            (None, None) => None,
        }
    }

    #[cfg(test)]
    pub fn serial_txd_pin_high(&self) -> bool {
        !self.uart_break_active() && self.serial_tx_pin_high
    }

    pub fn next_audio_irq_cck(&self, dmacon: u16) -> Option<u32> {
        // Only the period counter raises interrupts asynchronously (word
        // and half-word boundaries in states 010/011); DMA-arrival and
        // CPU-write interrupts are raised synchronously at the slot or
        // MMIO write. Bound the caller's step only when the upcoming
        // boundary can actually interrupt: IRQ-mode output interrupts at
        // every word start, DMA mode only around a length rollover
        // (intreq2 pending, or the final word is in flight) or in
        // attach-period mode (010 -> 011 raises in IRQ mode / rollover).
        self.chans
            .iter()
            .enumerate()
            .filter_map(|(ch_idx, ch)| {
                if !ch.outputting() {
                    return None;
                }
                let on = Self::aud_on(dmacon, ch_idx);
                let may_irq = !on || ch.intreq2 || ch.audlen <= 1 || self.aud_ap(ch_idx);
                if !may_irq {
                    return None;
                }
                // The 010 -> 011 boundary raises only in attach-period
                // mode; otherwise the earliest interrupting edge is the
                // following word start, one period later.
                let extra = if ch.state == AUD_OUT_HI && !self.aud_ap(ch_idx) {
                    if ch.per == 0 {
                        0x1_0000
                    } else {
                        u32::from(ch.per)
                    }
                } else {
                    0
                };
                Some(ch.percnt.saturating_add(extra).max(1))
            })
            .min()
    }

    fn load_serial_shift_if_idle(&mut self) -> u16 {
        if self.serial_tx_shift.is_some() {
            return 0;
        }
        let Some(word) = self.serial_tx_buffer.take() else {
            return 0;
        };
        let long = self.serial_long();
        let bit_cck = self.serial_bit_cck();
        let shift = SerialTxShift {
            word,
            long,
            bit_cck,
            remaining_cck: bit_cck,
            bit_index: 0,
            total_bits: Self::serial_tx_total_bits(word, long),
            break_seen: self.uart_break_active(),
        };
        self.serial_tx_pin_high = Self::serial_tx_bit(&shift);
        self.serial_tx_shift = Some(shift);
        INT_TBE
    }

    fn load_serial_rx_shift_if_idle(&mut self, long: bool) {
        if self.serial_rx_shift.is_some() {
            return;
        }
        let Some(word) = self.serial.read_word(long) else {
            return;
        };
        let bit_cck = self.serial_bit_cck();
        let shift = SerialRxShift {
            word,
            long,
            bit_cck,
            remaining_cck: bit_cck,
            bit_index: 0,
            total_bits: if long { 11 } else { 10 },
        };
        self.serial_rx_pin_high = Self::serial_rx_bit(&shift);
        self.serial_rx_shift = Some(shift);
    }

    fn serial_long(&self) -> bool {
        self.serper & SERPER_LONG != 0
    }

    fn serial_bit_cck(&self) -> u32 {
        u32::from(self.serper & 0x7FFF).saturating_add(1).max(1)
    }

    fn uart_break_active(&self) -> bool {
        self.adkcon & ADKCON_UARTBRK != 0
    }

    fn serial_tx_total_bits(word: u16, long: bool) -> u8 {
        if word == 0 {
            return if long { 11 } else { 10 };
        }
        let highest = u16::BITS as u8 - 1 - word.leading_zeros() as u8;
        1 + highest + 1
    }

    fn serial_tx_bit(shift: &SerialTxShift) -> bool {
        if shift.bit_index == 0 {
            false
        } else {
            shift.word & (1u16 << (shift.bit_index - 1)) != 0
        }
    }

    fn serial_tx_data_word(shift: &SerialTxShift) -> u16 {
        if shift.long {
            shift.word & 0x01FF
        } else {
            shift.word & 0x00FF
        }
    }

    fn serial_rx_bit(shift: &SerialRxShift) -> bool {
        if shift.bit_index == 0 {
            false
        } else {
            let data_bits = if shift.long { 9 } else { 8 };
            if shift.bit_index <= data_bits {
                shift.word & (1u16 << (shift.bit_index - 1)) != 0
            } else {
                true
            }
        }
    }

    fn advance_serial_rx_synchronizer(&mut self, cck: u32) {
        for _ in 0..cck.min(2) {
            self.serial_rx_sync_1_high = self.serial_rx_sync_0_high;
            self.serial_rx_sync_0_high = self.serial_rx_pin_high;
        }
    }

    fn serdatr_receive_word(word: u16, long: bool) -> u16 {
        if long {
            (word & 0x01FF) | 0x0200
        } else {
            (word & 0x00FF) | 0x0300
        }
    }

    /// Audio register write. `reg_off` is the offset within the
    /// $DFF0A0..$DFF0DF audio block (i.e. addr - $DFF0A0). Each
    /// channel occupies 16 bytes; the per-channel layout is:
    /// `+0 LCH +2 LCL +4 LEN +6 PER +8 VOL +A DAT`.
    pub fn write_audio_reg(&mut self, reg_off: u16, val: u16, dmacon: u16) {
        let ch_idx = (reg_off / 0x10) as usize;
        if ch_idx >= 4 {
            return;
        }
        let ptr_mask = self.dma_ptr_mask();
        let ch = &mut self.chans[ch_idx];
        match reg_off & 0x0F {
            0x0 => {
                // AUDxLCH: high 5 bits of chip-RAM address.
                ch.lc = ((ch.lc & 0x0000_FFFF) | (((val as u32) & 0x001F) << 16)) & ptr_mask;
            }
            0x2 => {
                // AUDxLCL: low 15 bits, low bit cleared (word-aligned).
                ch.lc = ((ch.lc & 0xFFFF_0000) | ((val as u32) & 0xFFFE)) & ptr_mask;
            }
            0x4 => {
                ch.len = val;
            }
            0x6 => {
                ch.per = val;
            }
            0x8 => {
                // AUDxVOL bits 0..6, max 64. Latch only; the live volume
                // reloads at the next output word start (volcntrld).
                ch.vol = paula_volume_from_word(val);
            }
            0xA => {
                // AUDxDAT: drives the state machine directly, exactly like
                // a DMA word arriving (the DMA slot writes this register).
                let irq = self.aud_poke_dat(ch_idx, val, dmacon);
                self.latch_interrupt_sources(irq);
            }
            _ => {}
        }
    }

    /// Audio register read. AUDxDAT reads return 0 on real hardware
    /// (it's write-only). We return 0 for everything in this block.
    pub fn read_audio_reg(&self, _reg_off: u16) -> u16 {
        0
    }

    pub fn peek_audio_reg_latch(&self, reg_off: u16) -> Option<u16> {
        let ch_idx = (reg_off / 0x10) as usize;
        if ch_idx >= 4 {
            return None;
        }
        let ch = &self.chans[ch_idx];
        match reg_off & 0x0F {
            0x0 => Some(((ch.lc >> 16) & 0x001F) as u16),
            0x2 => Some((ch.lc & 0xFFFE) as u16),
            0x4 => Some(ch.len),
            0x6 => Some(ch.per),
            0x8 => Some(ch.vol as u16),
            0xA => Some(ch.dat_latch),
            _ => None,
        }
    }

    fn dma_ptr_mask(&self) -> u32 {
        self.dma_addr_mask & !1
    }

    /// Advance Paula's audio state by `cck` color clocks and emit
    /// interleaved stereo frames to the AudioSink. Audio DMA memory
    /// words are supplied separately through `grant_audio_dma`, which
    /// lets Agnus own the documented channel slots.
    pub fn advance_audio(&mut self, cck: u32, dmacon: u16) -> u16 {
        let mut irq_bits = 0;
        let mut remaining = cck;
        while remaining > 0 {
            let step = remaining.min(self.cck_until_next_output_frame());
            irq_bits |= self.advance_audio_channels(step, dmacon);
            self.host_sample_acc += step as u64 * MIX_SAMPLE_RATE as u64;
            remaining -= step;

            while self.host_sample_acc >= PAULA_CLOCK_HZ as u64 {
                self.host_sample_acc -= PAULA_CLOCK_HZ as u64;
                self.push_mixed_frame();
            }
        }

        irq_bits
    }

    // ---- HRM state-machine terms (the appendix's signal names) ----

    /// AUDxON: the channel runs in DMA mode (DMACON master + channel bit).
    fn aud_on(dmacon: u16, ch: usize) -> bool {
        dmacon & DMACON_DMAEN != 0 && dmacon & (1 << ch) != 0
    }

    /// AUDxIP: the channel's interrupt is pending.
    fn aud_ip(&self, ch: usize) -> bool {
        self.intreq & INT_AUDX[ch] != 0
    }

    /// AUDxAV: attach-volume (this channel's words modulate ch+1's volume).
    fn aud_av(&self, ch: usize) -> bool {
        self.adkcon & (1 << ch) != 0
    }

    /// AUDxAP: attach-period (this channel's words modulate ch+1's period).
    fn aud_ap(&self, ch: usize) -> bool {
        self.adkcon & (1 << (ch + 4)) != 0
    }

    /// napnav: normal DMA/interrupt requests happen at word starts (true
    /// unless the channel is attach-period without attach-volume).
    fn aud_napnav(&self, ch: usize) -> bool {
        !self.aud_ap(ch) || self.aud_av(ch)
    }

    /// percntrld: reload the period counter (period 0 counts 65536).
    fn aud_percntrld(&mut self, ch: usize) {
        let per = self.chans[ch].per;
        self.chans[ch].percnt = if per == 0 { 0x1_0000 } else { u32::from(per) };
    }

    fn aud_volcntrld(&mut self, ch: usize) {
        self.chans[ch].audvol = self.chans[ch].vol;
    }

    fn aud_lencntrld(&mut self, ch: usize) {
        self.chans[ch].audlen = self.chans[ch].len;
    }

    fn aud_lencount(&mut self, ch: usize) {
        self.chans[ch].audlen = self.chans[ch].audlen.wrapping_sub(1);
    }

    /// lenfin: the length counter is on its final word.
    fn aud_lenfin(&self, ch: usize) -> bool {
        self.chans[ch].audlen == 1
    }

    /// AUDxDSR: reset the DMA pointer to the block start (AUDxLC).
    fn aud_dsr(&mut self, ch: usize) {
        self.chans[ch].ptr = self.chans[ch].lc & self.dma_ptr_mask();
    }

    /// pbufld1: load the output buffer from the AUDxDAT holding register.
    /// In attach-volume mode the word drives the next channel's volume
    /// latch instead and the buffer keeps its old value.
    fn aud_pbufld1(&mut self, ch: usize) {
        let dat = self.chans[ch].auddat;
        if !self.aud_av(ch) {
            self.chans[ch].buffer = dat;
        } else if ch < 3 {
            self.chans[ch + 1].vol = paula_volume_from_word(dat);
        }
    }

    /// pbufld2: in attach-period mode the word drives the next channel's
    /// period latch (taken on the 010 -> 011 transition).
    fn aud_pbufld2(&mut self, ch: usize) {
        let dat = self.chans[ch].auddat;
        if ch < 3 {
            self.chans[ch + 1].per = dat;
        }
    }

    /// penhi/penlo: gate the buffer's high/low byte into the DAC.
    fn aud_penhi(&mut self, ch: usize) {
        self.chans[ch].current = (self.chans[ch].buffer >> 8) as u8 as i8;
    }

    fn aud_penlo(&mut self, ch: usize) {
        self.chans[ch].current = self.chans[ch].buffer as u8 as i8;
    }

    // ---- HRM state transitions. Each returns the INTREQ bits it raises;
    // callers latch them (they are also latched here so AUDxIP tests
    // later in the same advance span see them). ----

    /// 000 -> 001 (DMA mode): DMA switched on; request the first word.
    fn aud_move_000_001(&mut self, ch: usize) -> u16 {
        self.aud_lencntrld(ch);
        self.chans[ch].sm_dr = true;
        self.chans[ch].state = AUD_DMA_FIRST;
        0
    }

    /// 000 -> 010 (IRQ mode): a CPU AUDxDAT write starts direct output.
    fn aud_move_000_010(&mut self, ch: usize) -> u16 {
        self.aud_volcntrld(ch);
        self.aud_percntrld(ch);
        self.aud_pbufld1(ch);
        self.chans[ch].state = AUD_OUT_HI;
        self.aud_penhi(ch);
        self.latch_interrupt_sources(INT_AUDX[ch]);
        INT_AUDX[ch]
    }

    /// 001 -> 101 (DMA mode): the first start-up word arrived. Raise the
    /// channel interrupt, request the next word, and point the DMA
    /// pointer at the block start (this first fetch used the stale
    /// pointer; its word is never played).
    fn aud_move_001_101(&mut self, ch: usize) -> u16 {
        self.chans[ch].sm_dr = true;
        self.aud_dsr(ch);
        if !self.aud_lenfin(ch) {
            self.aud_lencount(ch);
        }
        self.chans[ch].state = AUD_DMA_SECOND;
        self.latch_interrupt_sources(INT_AUDX[ch]);
        INT_AUDX[ch]
    }

    /// 101 -> 010 (DMA mode): the second word arrived; output begins.
    fn aud_move_101_010(&mut self, ch: usize) -> u16 {
        self.aud_percntrld(ch);
        self.aud_volcntrld(ch);
        self.aud_pbufld1(ch);
        if self.aud_napnav(ch) {
            self.chans[ch].sm_dr = true;
        }
        self.chans[ch].state = AUD_OUT_HI;
        self.aud_penhi(ch);
        0
    }

    /// 010 -> 011: period expired on the high byte; output the low byte.
    fn aud_move_010_011(&mut self, ch: usize, dmacon: u16) -> u16 {
        self.aud_percntrld(ch);
        let mut irq = 0;
        if self.aud_ap(ch) {
            self.aud_pbufld2(ch);
            if Self::aud_on(dmacon, ch) {
                self.chans[ch].sm_dr = true;
                if self.chans[ch].intreq2 {
                    irq |= INT_AUDX[ch];
                    self.chans[ch].intreq2 = false;
                }
            } else {
                irq |= INT_AUDX[ch];
            }
        }
        self.chans[ch].state = AUD_OUT_LO;
        self.aud_penlo(ch);
        self.latch_interrupt_sources(irq);
        irq
    }

    /// 011 -> 010: period expired on the low byte; start the next word.
    fn aud_move_011_010(&mut self, ch: usize, dmacon: u16) -> u16 {
        self.aud_percntrld(ch);
        self.aud_pbufld1(ch);
        self.aud_volcntrld(ch);
        let mut irq = 0;
        if self.aud_napnav(ch) {
            if Self::aud_on(dmacon, ch) {
                self.chans[ch].sm_dr = true;
                if self.chans[ch].intreq2 {
                    irq |= INT_AUDX[ch];
                    self.chans[ch].intreq2 = false;
                }
            } else {
                // IRQ mode: every word start raises the interrupt; the
                // 011 exit already required it to be acknowledged.
                irq |= INT_AUDX[ch];
            }
        }
        self.chans[ch].state = AUD_OUT_HI;
        self.aud_penhi(ch);
        self.latch_interrupt_sources(irq);
        irq
    }

    /// Any active state -> 000 (DMA switched off, or IRQ-mode output ended
    /// unacknowledged). The DAC holds the last byte; a posted DMA request
    /// stays posted (Agnus services it even with the channel bit off,
    /// which is what lets a brief on/off DMACON pulse kick a channel into
    /// free-running IRQ-mode output).
    fn aud_move_to_idle(&mut self, ch: usize) {
        self.chans[ch].state = AUD_IDLE;
        self.chans[ch].intreq2 = false;
    }

    /// AUDxDAT arrival: the state machine's main input, driven by both
    /// CPU/Copper writes and the channel's DMA slot.
    fn aud_poke_dat(&mut self, ch: usize, value: u16, dmacon: u16) -> u16 {
        self.chans[ch].auddat = value;
        self.chans[ch].dat_latch = value;
        if Self::aud_on(dmacon, ch) {
            match self.chans[ch].state {
                AUD_IDLE => self.aud_move_000_001(ch),
                AUD_DMA_FIRST => self.aud_move_001_101(ch),
                AUD_DMA_SECOND => self.aud_move_101_010(ch),
                AUD_OUT_HI | AUD_OUT_LO => {
                    // Steady-state fetch: count the word; on the final one
                    // reload the length/pointer and arm the rollover
                    // interrupt for the next word-start transition.
                    if !self.aud_lenfin(ch) {
                        self.aud_lencount(ch);
                    } else {
                        self.aud_lencntrld(ch);
                        self.aud_dsr(ch);
                        self.chans[ch].intreq2 = true;
                    }
                    0
                }
                _ => 0,
            }
        } else if self.chans[ch].state == AUD_IDLE && !self.aud_ip(ch) {
            self.aud_move_000_010(ch)
        } else {
            0
        }
    }

    /// Apply DMACON audio-channel edges. The caller flushes pending audio
    /// time first so the edge lands at the write's emulated moment.
    pub fn apply_audio_dmacon_edges(&mut self, old_dmacon: u16, new_dmacon: u16) {
        for ch in 0..4 {
            let was = Self::aud_on(old_dmacon, ch);
            let is = Self::aud_on(new_dmacon, ch);
            if was == is {
                continue;
            }
            if is {
                if self.chans[ch].state == AUD_IDLE {
                    self.aud_move_000_001(ch);
                }
            } else {
                // AUDxON falling edge. In the DMA start-up states (001/101)
                // the channel has not begun output, so idle it now; a DMA
                // request already posted survives and can still free-run the
                // channel (the "brief DMACON pulse" idiom). While the channel
                // is OUTPUTTING (010/011) the clear is NOT sampled here: real
                // Paula only re-evaluates AUDxON at the word-start boundary,
                // which the 011 period event already models (it idles on
                // AUDxON low AND AUDxIP set). Idling at the write instant
                // would let a clear/set pair shorter than the remaining word
                // restart the sample from AUDxLC instead of being missed --
                // the issue #74 regression (2c1.adf). vAmiga's synchronous
                // disableDMA() idles immediately here and gets this wrong too.
                match self.chans[ch].state {
                    AUD_DMA_FIRST | AUD_DMA_SECOND => self.aud_move_to_idle(ch),
                    _ => {}
                }
            }
        }
    }

    /// Transfer state-machine DMA requests to the Agnus-side latches.
    /// Runs once per scanline at the line end, mirroring real Paula
    /// (requests are sampled into Agnus during the line-start refresh
    /// slots and serviced at the channel's fixed slot on that line).
    pub fn transfer_audio_dma_requests(&mut self) {
        for ch in &mut self.chans {
            if ch.sm_dr {
                ch.sm_dr = false;
                ch.agnus_dr = true;
            }
        }
    }

    #[cfg(test)]
    pub fn tick_audio(&mut self, cck: u32, dmacon: u16, chip_ram: &[u8]) -> u16 {
        // Drive the FSM the way the bus does: per scanline, service each
        // channel's fixed DMA slot from the line-latched requests, advance
        // emulated time, and transfer fresh requests at the line end. The
        // DMACON edge is applied synchronously up front like the MMIO path.
        let mut irq_bits = 0;
        if dmacon != self.test_last_dmacon {
            let old = self.test_last_dmacon;
            self.test_last_dmacon = dmacon;
            self.apply_audio_dmacon_edges(old, dmacon);
        }
        for _ in 0..cck {
            let slot = match self.test_line_cck {
                0x00D => Some(0),
                0x00F => Some(1),
                0x011 => Some(2),
                0x013 => Some(3),
                _ => None,
            };
            if let Some(ch_idx) = slot {
                if let Some(request) = self.audio_dma_request(ch_idx) {
                    let word = read_chip_word_for_audio_test(chip_ram, request.address);
                    irq_bits |= self.grant_audio_dma(ch_idx, word, dmacon);
                }
            }
            irq_bits |= self.advance_audio(1, dmacon);
            self.test_line_cck += 1;
            if self.test_line_cck >= 227 {
                self.test_line_cck = 0;
                self.transfer_audio_dma_requests();
            }
        }
        irq_bits
    }

    fn cck_until_next_output_frame(&self) -> u32 {
        let needed = (PAULA_CLOCK_HZ as u64).saturating_sub(self.host_sample_acc);
        needed.div_ceil(MIX_SAMPLE_RATE as u64).max(1) as u32
    }

    /// The chip-RAM address the channel's next DMA slot will fetch, when a
    /// request is latched in Agnus.
    pub fn audio_dma_request(&self, ch_idx: usize) -> Option<AudioDmaRequest> {
        let ch = self.chans.get(ch_idx)?;
        if !ch.agnus_dr {
            return None;
        }
        Some(AudioDmaRequest {
            address: ch.ptr & self.dma_ptr_mask(),
        })
    }

    /// Service the channel's DMA slot: consume the Agnus request latch,
    /// advance the pointer, and feed the fetched word to the state
    /// machine. The slot runs regardless of the DMACON audio bits; only
    /// the request latch gates it.
    pub fn grant_audio_dma(&mut self, ch_idx: usize, word: u16, dmacon: u16) -> u16 {
        let ptr_mask = self.dma_ptr_mask();
        let Some(ch) = self.chans.get_mut(ch_idx) else {
            return 0;
        };
        if !ch.agnus_dr {
            return 0;
        }
        ch.agnus_dr = false;
        ch.ptr = ch.ptr.wrapping_add(2) & ptr_mask;
        let irq = self.aud_poke_dat(ch_idx, word, dmacon);
        self.latch_interrupt_sources(irq);
        irq
    }

    /// Advance the per-channel period counters, running the state machine
    /// at each expiry (states 010/011 are the only ones with the counter
    /// live). Returns the INTREQ bits raised (already latched).
    fn advance_audio_channels(&mut self, cck: u32, dmacon: u16) -> u16 {
        let mut irq_bits = 0;
        for ch_idx in 0..4 {
            if !self.chans[ch_idx].outputting() {
                continue;
            }
            let mut remaining = cck;
            while remaining > 0 {
                let ch = &mut self.chans[ch_idx];
                if !ch.outputting() {
                    break;
                }
                if ch.percnt > remaining {
                    ch.percnt -= remaining;
                    break;
                }
                remaining -= ch.percnt;
                ch.percnt = 0;
                irq_bits |= match ch.state {
                    AUD_OUT_HI => self.aud_move_010_011(ch_idx, dmacon),
                    _ => {
                        if Self::aud_on(dmacon, ch_idx) || !self.aud_ip(ch_idx) {
                            self.aud_move_011_010(ch_idx, dmacon)
                        } else {
                            // IRQ-mode output with the interrupt still
                            // pending: the channel parks.
                            self.aud_move_to_idle(ch_idx);
                            0
                        }
                    }
                };
            }
        }
        irq_bits
    }

    fn push_mixed_frame(&mut self) {
        // Mix and push host-rate stereo frames into the sink. Paula
        // stereo routing follows the common A500/A600/A1200 and
        // Minimig mapping: channels 0 and 3 go left, 1 and 2 right.
        // Some HRM prose and motherboard jack labels describe the
        // opposite physical side; keep this as channel-to-DAC routing.
        // Minimig also exposes PWM-gated per-channel samples, but its
        // mixed DAC output uses the linear volume multiplier. Keep the
        // host PCM path linear until modelling the alternate PWM/filter
        // path as an explicit analog output mode.
        // Volume range is 0..64; each channel sample is signed 8-bit
        // (-128..127). Scale into [-1.0, 1.0] approximately by
        // dividing by (128.0 * 64.0). We sum two channels per side
        // unclipped: worst case is +/-2.0 if both channels saturate
        // with full volume in opposite phase, which is essentially
        // never the case for real music.
        // Tap each channel's output level (DAC sample * volume, -128..127)
        // for the debugger oscilloscopes. This is pre-mute so a muted
        // channel's trace still shows its activity (drawn greyed).
        for i in 0..4 {
            let level = ((self.chans[i].current as i32 * self.chans[i].audvol as i32) / 64)
                .clamp(-128, 127);
            scope_push(&mut self.channel_scope[i], level as i8);
        }
        let l_raw = self.channel_mixed_sample(0) + self.channel_mixed_sample(3);
        let r_raw = self.channel_mixed_sample(1) + self.channel_mixed_sample(2);
        let scale = 1.0 / (128.0 * 64.0);
        let mut left = l_raw as f32 * scale;
        let mut right = r_raw as f32 * scale;
        let filtered = self.led_filter.process(left, right);
        if self.led_filter_enabled {
            (left, right) = filtered;
        }
        // Pure Paula-sum stem tap: post-LED-filter, pre-drive/CD/MT-32. Real
        // hardware's LED filter sits after the channel mixer's summation, so
        // the per-channel taps below are deliberately *not* filtered.
        self.audio.push_source("paula", left, right);
        let ch_scaled: [f32; 4] =
            std::array::from_fn(|i| self.channel_mixed_sample(i) as f32 * scale);
        self.audio.push_source_channel("paula", "0", ch_scaled[0]);
        self.audio.push_source_channel("paula", "1", ch_scaled[1]);
        self.audio.push_source_channel("paula", "2", ch_scaled[2]);
        self.audio.push_source_channel("paula", "3", ch_scaled[3]);
        // Drive noises join after the LED filter (acoustic, not part of
        // Paula's output path) but under the master volume control.
        let drive = self.drive_sounds.next_sample();
        left += drive;
        right += drive;
        self.audio.push_source("drivesounds", drive, drive);
        // CD audio (CD32/CDTV) is line-mixed with Paula's output after
        // the LED filter, like the real mixer stage, and also sits under
        // the master volume control.
        let (mut cd_left, mut cd_right) = self.cd_audio.next_sample();
        // Record the pre-mute CD level for the debugger scope, then apply
        // the developer CD mute so the trace still shows activity while the
        // stream is silenced in the mix.
        scope_push(&mut self.cd_scope, scope_level(cd_left, cd_right));
        if self.cd_muted {
            cd_left = 0.0;
            cd_right = 0.0;
        }
        left += cd_left;
        right += cd_right;
        // Stem tap reflects audible content, so it sits after the mute gate
        // (unlike the scope tap above, which stays pre-mute for visibility).
        self.audio.push_source("cdda", cd_left, cd_right);
        // A MIDI device emulated in-process (an MT-32) is line-mixed the same
        // way, so the Amiga's own voices keep playing under it exactly as
        // they would beside a real one on the desk. Scope pre-mute, stem and
        // mix post-mute, exactly like CD-DA above.
        let mut synth_left = 0.0f32;
        let mut synth_right = 0.0f32;
        if !self.synth_silent {
            match self.serial.next_audio_frame() {
                Some((sl, sr)) => {
                    synth_left = sl;
                    synth_right = sr;
                }
                None => self.synth_silent = true,
            }
        }
        scope_push(&mut self.synth_scope, scope_level(synth_left, synth_right));
        if self.synth_muted {
            synth_left = 0.0;
            synth_right = 0.0;
        }
        left += synth_left;
        right += synth_right;
        self.audio
            .push_source(self.serial.synth_source_name(), synth_left, synth_right);
        // A Toccata board resamples its own codec-rate output to the mixer
        // rate before pushing here (see `Toccata::tick`), so this is a
        // plain per-frame pop like CD-DA, not a rate conversion.
        let (mut toccata_left, mut toccata_right) = self.toccata_audio.next_sample();
        scope_push(
            &mut self.toccata_scope,
            scope_level(toccata_left, toccata_right),
        );
        if self.toccata_muted {
            toccata_left = 0.0;
            toccata_right = 0.0;
        }
        left += toccata_left;
        right += toccata_right;
        self.audio
            .push_source("toccata", toccata_left, toccata_right);
        // Likewise, an MHI board resamples its own decoded-MPEG-rate output
        // to the mixer rate before pushing here (see `Mhi::advance_mixer`).
        let (mut mhi_left, mut mhi_right) = self.mhi_audio.next_sample();
        scope_push(&mut self.mhi_scope, scope_level(mhi_left, mhi_right));
        if self.mhi_muted {
            mhi_left = 0.0;
            mhi_right = 0.0;
        }
        left += mhi_left;
        right += mhi_right;
        self.audio.push_source("mhi", mhi_left, mhi_right);
        if let Some(capture) = &mut self.capture {
            capture.push((left, right));
        }
        let (mut out_left, mut out_right) = (left * self.output_volume, right * self.output_volume);
        // Stereo width via mid/side: `sep` 1.0 leaves the hardware panning
        // untouched (out == in, the default), 0.0 collapses to mono. Mono mode
        // is just a forced 0. Skipped entirely at full width so the default path
        // is unchanged.
        let sep = if self.mono_output {
            0.0
        } else {
            self.stereo_separation
        };
        if sep < 1.0 {
            let mid = (out_left + out_right) * 0.5;
            let side = (out_left - out_right) * 0.5 * sep;
            out_left = mid + side;
            out_right = mid - side;
        }
        self.audio.push_master(out_left, out_right);
    }

    fn channel_mixed_sample(&self, ch_idx: usize) -> i32 {
        if self.channel_muted[ch_idx] || self.channel_attached_as_modulator(ch_idx) {
            0
        } else {
            let ch = &self.chans[ch_idx];
            (ch.current as i32) * (ch.audvol as i32)
        }
    }
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct StereoLedFilter {
    left: AnalogLedFilter,
    right: AnalogLedFilter,
}

impl StereoLedFilter {
    fn new() -> Self {
        Self {
            left: AnalogLedFilter::new(LED_FILTER_CUTOFF_HZ, MIX_SAMPLE_RATE as f32),
            right: AnalogLedFilter::new(LED_FILTER_CUTOFF_HZ, MIX_SAMPLE_RATE as f32),
        }
    }

    fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        (self.left.process(left), self.right.process(right))
    }
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct AnalogLedFilter {
    one_pole: OnePoleLowPass,
    two_pole: BiquadLowPass,
}

impl AnalogLedFilter {
    fn new(cutoff_hz: f32, sample_rate_hz: f32) -> Self {
        Self {
            one_pole: OnePoleLowPass::new(cutoff_hz, sample_rate_hz),
            two_pole: BiquadLowPass::new(cutoff_hz, sample_rate_hz),
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        self.two_pole.process(self.one_pole.process(input))
    }
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct OnePoleLowPass {
    alpha: f32,
    z: f32,
}

impl OnePoleLowPass {
    fn new(cutoff_hz: f32, sample_rate_hz: f32) -> Self {
        let alpha = 1.0 - (-2.0 * PI * cutoff_hz / sample_rate_hz).exp();
        Self { alpha, z: 0.0 }
    }

    fn process(&mut self, input: f32) -> f32 {
        self.z += self.alpha * (input - self.z);
        self.z
    }
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct BiquadLowPass {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl BiquadLowPass {
    fn new(cutoff_hz: f32, sample_rate_hz: f32) -> Self {
        let omega = 2.0 * PI * cutoff_hz / sample_rate_hz;
        let sin = omega.sin();
        let cos = omega.cos();
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let alpha = sin / (2.0 * q);

        let b0 = (1.0 - cos) * 0.5;
        let b1 = 1.0 - cos;
        let b2 = b0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos;
        let a2 = 1.0 - alpha;

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

    fn process(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }
}

impl Drop for Paula {
    fn drop(&mut self) {
        self.audio.flush();
        self.serial.flush();
    }
}

/// Map a set of pending+enabled Paula interrupt bits to a 68K IPL.
/// Returns 0 if nothing is pending. The mapping is fixed by the
/// hardware: the chipset wires each interrupt line to a specific CPU
/// IPL level (see Amiga Hardware Reference Manual, Paula chapter).
pub fn pending_ipl(pending: u16) -> u8 {
    // EXTER, plus the undocumented INT14 source which shares EXTER's
    // IPL in the Paula RTL.
    if pending & (INT_INT14 | (1 << 13)) != 0 {
        6
    }
    // DSKSYN, RBF
    else if pending & ((1 << 12) | (1 << 11)) != 0 {
        5
    }
    // AUD0..AUD3 (bits 7..10)
    else if pending & 0x0780 != 0 {
        4
    }
    // BLIT, VERTB, COPER
    else if pending & ((1 << 6) | (1 << 5) | (1 << 4)) != 0 {
        3
    }
    // PORTS
    else if pending & (1 << 3) != 0 {
        2
    }
    // SOFT, DSKBLK, TBE
    else if pending & ((1 << 2) | (1 << 1) | (1 << 0)) != 0 {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::WavSink;
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::process;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    struct NoopSerial;

    impl SerialSink for NoopSerial {
        fn write_byte(&mut self, _b: u8, _at_cck: u64) {}
        fn flush(&mut self) {}
    }

    struct CollectSerial {
        written: Arc<Mutex<Vec<u8>>>,
        read: Arc<Mutex<Vec<u8>>>,
    }

    impl SerialSink for CollectSerial {
        fn write_byte(&mut self, b: u8, _at_cck: u64) {
            self.written.lock().unwrap().push(b);
        }

        fn read_byte(&mut self) -> Option<u8> {
            let mut read = self.read.lock().unwrap();
            if read.is_empty() {
                None
            } else {
                Some(read.remove(0))
            }
        }

        fn has_pending_input(&self) -> bool {
            !self.read.lock().unwrap().is_empty()
        }

        fn flush(&mut self) {}
    }

    struct CollectSerialWords {
        written: Arc<Mutex<Vec<(u16, bool)>>>,
        read: Arc<Mutex<Vec<u16>>>,
    }

    impl SerialSink for CollectSerialWords {
        fn write_byte(&mut self, b: u8, _at_cck: u64) {
            self.written.lock().unwrap().push((u16::from(b), false));
        }

        fn write_word(&mut self, word: u16, long: bool, _at_cck: u64) {
            self.written.lock().unwrap().push((word, long));
        }

        fn read_word(&mut self, _long: bool) -> Option<u16> {
            let mut read = self.read.lock().unwrap();
            if read.is_empty() {
                None
            } else {
                Some(read.remove(0))
            }
        }

        fn has_pending_input(&self) -> bool {
            !self.read.lock().unwrap().is_empty()
        }

        fn flush(&mut self) {}
    }

    /// Records each transmitted word alongside the emit-time color clock the
    /// UART stamped it with, so the emit-time plumbing can be asserted.
    struct TimedSerial {
        events: Arc<Mutex<Vec<(u16, u64)>>>,
    }

    impl SerialSink for TimedSerial {
        fn write_byte(&mut self, b: u8, at_cck: u64) {
            self.events.lock().unwrap().push((u16::from(b), at_cck));
        }

        fn write_word(&mut self, word: u16, _long: bool, at_cck: u64) {
            self.events.lock().unwrap().push((word, at_cck));
        }

        fn flush(&mut self) {}
    }

    type WordSerialFixture = (Paula, Arc<Mutex<Vec<(u16, bool)>>>, Arc<Mutex<Vec<u16>>>);

    struct CollectSink {
        frames: Rc<RefCell<Vec<(f32, f32)>>>,
    }

    impl AudioSink for CollectSink {
        fn push(&mut self, left: f32, right: f32) {
            self.frames.borrow_mut().push((left, right));
        }

        fn flush(&mut self) {}
    }

    type SharedFrames = Rc<RefCell<Vec<(f32, f32)>>>;

    fn paula_with_collect_sink() -> (Paula, SharedFrames) {
        let frames = Rc::new(RefCell::new(Vec::new()));
        let audio = CollectSink {
            frames: Rc::clone(&frames),
        };
        (Paula::new(Box::new(NoopSerial), Box::new(audio)), frames)
    }

    type SharedBytes = Arc<Mutex<Vec<u8>>>;

    fn paula_with_collect_serial() -> (Paula, SharedBytes, SharedBytes) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let read = Arc::new(Mutex::new(Vec::new()));
        let serial = CollectSerial {
            written: Arc::clone(&written),
            read: Arc::clone(&read),
        };
        (
            Paula::new(Box::new(serial), Box::new(NullAudio)),
            written,
            read,
        )
    }

    fn paula_with_collect_serial_words() -> WordSerialFixture {
        let written = Arc::new(Mutex::new(Vec::new()));
        let read = Arc::new(Mutex::new(Vec::new()));
        let serial = CollectSerialWords {
            written: Arc::clone(&written),
            read: Arc::clone(&read),
        };
        (
            Paula::new(Box::new(serial), Box::new(NullAudio)),
            written,
            read,
        )
    }

    struct NullAudio;

    impl AudioSink for NullAudio {
        fn push(&mut self, _left: f32, _right: f32) {}
        fn flush(&mut self) {}
    }

    #[test]
    fn audio_filter_mode_overrides_the_guest_led_line() {
        use crate::config::AudioFilterMode;
        let (mut paula, _frames) = paula_with_collect_sink();
        // Auto follows the guest /LED line.
        paula.set_led_filter_mode(AudioFilterMode::Auto);
        paula.set_led_filter_guest(true);
        assert!(paula.led_filter_enabled());
        paula.set_led_filter_guest(false);
        assert!(!paula.led_filter_enabled());
        // On forces the filter engaged whatever the guest asks.
        paula.set_led_filter_mode(AudioFilterMode::On);
        assert!(paula.led_filter_enabled());
        paula.set_led_filter_guest(false);
        assert!(paula.led_filter_enabled());
        // Off forces it bypassed whatever the guest asks.
        paula.set_led_filter_mode(AudioFilterMode::Off);
        paula.set_led_filter_guest(true);
        assert!(!paula.led_filter_enabled());
    }

    #[test]
    fn audio_capture_tap_mirrors_sink_frames_before_master_volume() {
        let (mut paula, frames) = paula_with_collect_sink();
        paula.set_led_filter_guest(false);
        paula.set_output_volume_percent(50);
        let mut ram = vec![0u8; 512 * 1024];
        ram[0] = 0x7F;
        ram[1] = 0x81;

        paula.write_audio_reg(0x00, 0, 0);
        paula.write_audio_reg(0x02, 0, 0);
        paula.write_audio_reg(0x04, 1, 0);
        paula.write_audio_reg(0x06, 80, 0);
        paula.write_audio_reg(0x08, 64, 0);

        // Tap disabled: nothing accumulates.
        paula.tick_audio(400, DMACON_DMAEN | 0x0001, &ram);
        assert!(paula.take_captured_audio().is_empty());

        paula.set_audio_capture_enabled(true);
        paula.tick_audio(400, DMACON_DMAEN | 0x0001, &ram);
        let captured = paula.take_captured_audio();
        let sink_frames = frames.borrow();
        assert!(!captured.is_empty());
        // The tap carries the same mixed frames as the sink, but before
        // the master output volume (sink got 50%).
        let tail = &sink_frames[sink_frames.len() - captured.len()..];
        for ((cap_l, cap_r), (sink_l, sink_r)) in captured.iter().zip(tail) {
            assert!((cap_l * 0.5 - sink_l).abs() < 1e-6);
            assert!((cap_r * 0.5 - sink_r).abs() < 1e-6);
        }
        drop(sink_frames);

        // Draining empties the buffer; disabling discards new frames.
        assert!(paula.take_captured_audio().is_empty());
        paula.set_audio_capture_enabled(false);
        paula.tick_audio(400, DMACON_DMAEN | 0x0001, &ram);
        assert!(paula.take_captured_audio().is_empty());
    }

    #[test]
    fn large_audio_tick_emits_chronological_samples() {
        let (mut paula, frames) = paula_with_collect_sink();
        paula.set_led_filter_guest(false);
        let mut ram = vec![0u8; 512 * 1024];
        ram[0] = 0x7F;
        ram[1] = 0x81;
        ram[2] = 0x7F;
        ram[3] = 0x81;

        paula.write_audio_reg(0x00, 0, 0);
        paula.write_audio_reg(0x02, 0, 0);
        paula.write_audio_reg(0x04, 2, 0);
        paula.write_audio_reg(0x06, 80, 0);
        paula.write_audio_reg(0x08, 64, 0);

        paula.tick_audio(1600, DMACON_DMAEN | 0x0001, &ram);

        let frames = frames.borrow();
        assert!(
            frames.len() >= 4,
            "expected several output frames, got {}",
            frames.len()
        );
        let unique_left: BTreeSet<i32> = frames
            .iter()
            .map(|(left, _)| (left * 10_000.0).round() as i32)
            .collect();
        assert!(
            unique_left.len() > 1,
            "output frames should reflect byte changes inside the tick: {frames:?}"
        );
    }

    #[test]
    fn audio_irq_deadline_tracks_dma_buffer_rollover() {
        let (mut paula, _) = paula_with_collect_sink();
        let mut ram = vec![0u8; 64];
        ram[0] = 0x11;
        ram[1] = 0x22;
        ram[2] = 0x33;
        ram[3] = 0x44;
        let dmacon = DMACON_DMAEN | 0x0001;

        paula.write_audio_reg(0x00, 0, 0);
        paula.write_audio_reg(0x02, 0, 0);
        paula.write_audio_reg(0x04, 2, 0);
        paula.write_audio_reg(0x06, 10, 0);
        paula.write_audio_reg(0x08, 64, 0);

        // Idle: no asynchronous interrupt is possible.
        assert_eq!(paula.next_audio_irq_cck(dmacon), None);

        // The two start-up fetches raise the start interrupt (slot-timed,
        // no deadline needed) and leave the final word in flight
        // (audlen 1), so the period counter's boundaries now carry the
        // rollover interrupt and the deadline engages.
        let irq = paula.tick_audio(2 * 227 + 20, dmacon, &ram);
        assert_eq!(irq & INT_AUD0, INT_AUD0);
        assert!(paula.chans[0].outputting());
        assert_eq!(paula.chans[0].audlen, 1);
        let deadline = paula.next_audio_irq_cck(dmacon).expect("deadline");
        assert!(deadline <= 2 * 10, "deadline within one word: {deadline}");

        // The rollover interrupt arrives at a word-start boundary.
        assert!(!paula.write_intreq(INT_AUD0));
        let irq = paula.tick_audio(2 * 227 + 40, dmacon, &ram);
        assert_eq!(irq & INT_AUD0, INT_AUD0);
    }

    #[test]
    fn cpu_auddat_with_dma_disabled_outputs_high_then_low_byte() {
        let (mut paula, _) = paula_with_collect_sink();
        let ram = vec![0u8; 64];

        paula.write_audio_reg(0x06, 4, 0);
        paula.write_audio_reg(0x08, 64, 0);
        // The write starts IRQ-mode output immediately: the interrupt is
        // raised as the word is taken and the high byte hits the DAC.
        paula.write_audio_reg(0x0A, 0x4080, 0);
        assert_eq!(paula.intreq & INT_AUD0, INT_AUD0);
        assert_eq!(paula.chans[0].current, 0x40);
        assert_eq!(paula.chans[0].state, AUD_OUT_HI);

        // One period later the low byte plays.
        paula.tick_audio(4, 0, &ram);
        assert_eq!(paula.chans[0].current, 0x80u8 as i8);
        assert_eq!(paula.chans[0].state, AUD_OUT_LO);

        // The interrupt was never acknowledged, so the word's end parks
        // the channel; the DAC holds the last level.
        paula.tick_audio(4, 0, &ram);
        assert_eq!(paula.chans[0].state, AUD_IDLE);
        assert_eq!(paula.chans[0].current, 0x80u8 as i8);
        paula.tick_audio(16, 0, &ram);
        assert_eq!(paula.chans[0].current, 0x80u8 as i8);
    }

    #[test]
    fn cpu_auddat_acknowledged_stream_plays_seamlessly() {
        let (mut paula, _) = paula_with_collect_sink();
        let ram = vec![0u8; 64];

        paula.write_audio_reg(0x06, 8, 0);
        paula.write_audio_reg(0x0A, 0x1020, 0);
        assert_eq!(paula.chans[0].current, 0x10);
        // Acknowledge and supply the next word before this one ends: the
        // channel keeps cycling, loading the fresh word at the boundary.
        assert!(!paula.write_intreq(INT_AUD0));
        paula.write_audio_reg(0x0A, 0x3040, 0);
        paula.tick_audio(8, 0, &ram);
        assert_eq!(paula.chans[0].current, 0x20);
        assert_eq!(paula.chans[0].state, AUD_OUT_LO);
        // Word boundary: the new word starts and re-raises the interrupt.
        paula.tick_audio(8, 0, &ram);
        assert_eq!(paula.chans[0].current, 0x30);
        assert_eq!(paula.intreq & INT_AUD0, INT_AUD0);
        assert!(!paula.write_intreq(INT_AUD0));
        paula.tick_audio(8, 0, &ram);
        assert_eq!(paula.chans[0].current, 0x40);
    }

    #[test]
    fn cpu_auddat_write_waits_for_audio_interrupt_clear_in_manual_mode() {
        let (mut paula, _) = paula_with_collect_sink();
        let ram = vec![0u8; 64];

        paula.write_audio_reg(0x06, 4, 0);
        paula.write_audio_reg(0x0A, 0x1020, 0);
        assert_eq!(paula.intreq & INT_AUD0, INT_AUD0);
        // Let the word play out unacknowledged: the channel parks.
        paula.tick_audio(8, 0, &ram);
        assert_eq!(paula.chans[0].state, AUD_IDLE);
        assert_eq!(paula.chans[0].current, 0x20);

        // With the interrupt still pending, a new AUDxDAT write does not
        // start output.
        paula.write_audio_reg(0x0A, 0x3040, 0);
        assert_eq!(paula.chans[0].state, AUD_IDLE);
        assert_eq!(paula.chans[0].current, 0x20);

        // Acknowledged, the next write starts a fresh word.
        assert!(!paula.write_intreq(INT_AUD0));
        paula.write_audio_reg(0x0A, 0x3040, 0);
        assert_eq!(paula.chans[0].state, AUD_OUT_HI);
        assert_eq!(paula.chans[0].current, 0x30);
    }

    #[test]
    fn audio_dma_length_one_loops_and_honors_repointed_location() {
        let (mut paula, _) = paula_with_collect_sink();
        let dmacon = DMACON_DMAEN | 0x0001;

        paula.write_audio_reg(0x00, 0, 0);
        paula.write_audio_reg(0x02, 0, 0);
        paula.write_audio_reg(0x04, 1, 0);
        paula.write_audio_reg(0x06, 4, 0);
        paula.write_audio_reg(0x08, 64, 0);

        // Start-up: dummy fetch (stale pointer) raises the start IRQ and
        // resets the pointer to AUDxLC.
        paula.apply_audio_dmacon_edges(0, dmacon);
        paula.transfer_audio_dma_requests();
        assert_eq!(
            paula.grant_audio_dma(0, 0xDEAD, dmacon) & INT_AUD0,
            INT_AUD0
        );
        // Second start-up fetch: the one-word block's word plays.
        paula.transfer_audio_dma_requests();
        assert_eq!(paula.audio_dma_request(0).unwrap().address, 0);
        assert_eq!(paula.grant_audio_dma(0, 0x1122, dmacon), 0);
        assert_eq!(paula.chans[0].current, 0x11);

        // The interrupt handler repoints the channel at a new block; the
        // next steady-state fetch is the final word (audlen 1), so its
        // arrival reloads pointer/length from the fresh latches.
        paula.write_audio_reg(0x02, 8, dmacon);
        paula.write_audio_reg(0x04, 2, dmacon);
        assert_eq!(paula.chans[0].audlen, 1);
        paula.transfer_audio_dma_requests();
        assert_eq!(paula.audio_dma_request(0).unwrap().address, 2);
        assert_eq!(paula.grant_audio_dma(0, 0x3344, dmacon), 0);
        assert!(paula.chans[0].intreq2);
        assert_eq!(paula.chans[0].audlen, 2);
        assert_eq!(paula.chans[0].ptr, 8);
    }

    #[test]
    fn audio_dma_length_zero_and_one_play_audibly() {
        // Real Paula has no length-based muting: AUDxLEN=0 latches a
        // 65536-word block (the counter wraps) and AUDxLEN=1 loops a
        // single word, both playing their fetched data. The CD32 boot
        // jingle is a LEN=0 one-shot; muting it left the boot silent.
        for len in [0u16, 1] {
            let (mut paula, _) = paula_with_collect_sink();
            let mut ram = vec![0u8; 64];
            ram[0] = 0x7F;
            ram[1] = 0x81;
            let dmacon = DMACON_DMAEN | 0x0001;

            paula.write_audio_reg(0x00, 0, 0);
            paula.write_audio_reg(0x02, 0, 0);
            paula.write_audio_reg(0x04, len, 0);
            paula.write_audio_reg(0x06, 4, 0);
            paula.write_audio_reg(0x08, 64, 0);

            // Two scanlines of start-up fetches, then output runs.
            let irq = paula.tick_audio(2 * 227 + 20, dmacon, &ram);
            assert_eq!(irq & INT_AUD0, INT_AUD0);
            assert!(paula.chans[0].outputting());
            assert_eq!(paula.chans[0].buffer, 0x7F81);
            let seen: Vec<i8> = (0..8)
                .map(|_| {
                    paula.tick_audio(4, dmacon, &ram);
                    paula.chans[0].current
                })
                .collect();
            assert!(seen.contains(&0x7F), "high byte should play: {seen:?}");
            assert!(
                seen.contains(&(0x81u8 as i8)),
                "low byte should play: {seen:?}"
            );
        }
    }

    #[test]
    fn dmacon_pulse_kicks_channel_into_irq_mode_free_run() {
        // vAmigaTS Paula/Audio/simple/pertimer1: software pulses AUD0's
        // DMACON bit on and immediately off (no AUDxDAT write, no audio
        // data intended). The enable edge posts a DMA request; the request
        // latch is NOT gated by DMACON, so Agnus still services it. The
        // arriving word then hits an idle channel in IRQ mode and starts
        // free-running output that raises the channel interrupt every
        // word, at the AUDxPER cadence -- a CPU-visible periodic timer.
        let (mut paula, _) = paula_with_collect_sink();
        let ram = vec![0u8; 64];
        let dmacon = DMACON_DMAEN | 0x0001;

        paula.write_audio_reg(0x06, 100, 0);
        paula.apply_audio_dmacon_edges(0, dmacon);
        paula.apply_audio_dmacon_edges(dmacon, 0);
        assert_eq!(paula.chans[0].state, AUD_IDLE);
        assert!(
            paula.chans[0].sm_dr,
            "the posted request survives the off edge"
        );

        // Line end: the request reaches Agnus; the slot fetches with the
        // channel bit off and the word starts IRQ-mode output.
        paula.transfer_audio_dma_requests();
        assert!(paula.audio_dma_request(0).is_some());
        assert_eq!(paula.grant_audio_dma(0, 0x1234, 0) & INT_AUD0, INT_AUD0);
        assert_eq!(paula.chans[0].state, AUD_OUT_HI);

        // Acknowledged each time, the channel keeps interrupting once per
        // word (2 * AUDxPER clocks).
        for _ in 0..3 {
            assert!(!paula.write_intreq(INT_AUD0));
            let irq = paula.tick_audio(2 * 100, 0, &ram);
            assert_eq!(irq & INT_AUD0, INT_AUD0);
        }

        // Left unacknowledged, the output parks at the word end.
        let _ = paula.tick_audio(2 * 100, 0, &ram);
        assert_eq!(paula.chans[0].state, AUD_IDLE);
    }

    #[test]
    fn audio_dma_startup_fetches_stale_pointer_then_block_start() {
        let (mut paula, _) = paula_with_collect_sink();
        let dmacon = DMACON_DMAEN | 0x0001;

        // The enable edge does NOT reload the pointer: the first start-up
        // fetch reads wherever it last stopped, and its word is discarded.
        paula.chans[0].ptr = 0x20;
        paula.write_audio_reg(0x00, 0, 0);
        paula.write_audio_reg(0x02, 4, 0);
        paula.write_audio_reg(0x04, 2, 0);
        paula.write_audio_reg(0x06, 8, 0);

        paula.apply_audio_dmacon_edges(0, dmacon);
        assert_eq!(paula.chans[0].state, AUD_DMA_FIRST);
        assert_eq!(paula.chans[0].audlen, 2);
        // The request reaches Agnus at the line end.
        assert!(paula.audio_dma_request(0).is_none());
        paula.transfer_audio_dma_requests();
        assert_eq!(paula.audio_dma_request(0).unwrap().address, 0x20);

        // First word arrives: start IRQ, pointer reset to AUDxLC, length
        // counted, next request posted. Nothing plays yet.
        assert_eq!(
            paula.grant_audio_dma(0, 0xDEAD, dmacon) & INT_AUD0,
            INT_AUD0
        );
        assert_eq!(paula.chans[0].state, AUD_DMA_SECOND);
        assert_eq!(paula.chans[0].ptr, 4);
        assert_eq!(paula.chans[0].audlen, 1);
        assert_eq!(paula.chans[0].current, 0);

        // Second word (from AUDxLC) starts output.
        paula.transfer_audio_dma_requests();
        assert_eq!(paula.audio_dma_request(0).unwrap().address, 4);
        assert_eq!(paula.grant_audio_dma(0, 0x5566, dmacon), 0);
        assert_eq!(paula.chans[0].state, AUD_OUT_HI);
        assert_eq!(paula.chans[0].current, 0x55);
        assert_eq!(paula.chans[0].ptr, 6);
    }

    #[test]
    fn audio_dma_rollover_reloads_at_final_fetch_and_interrupts_at_word_start() {
        let (mut paula, _) = paula_with_collect_sink();
        let mut ram = vec![0u8; 64];
        ram[0] = 0x10;
        ram[1] = 0x20;
        ram[2] = 0x30;
        ram[3] = 0x40;
        let dmacon = DMACON_DMAEN | 0x0001;

        // Two-word block; get output running via the test bench.
        paula.write_audio_reg(0x00, 0, 0);
        paula.write_audio_reg(0x02, 0, 0);
        paula.write_audio_reg(0x04, 2, 0);
        paula.write_audio_reg(0x06, 200, 0);
        paula.write_audio_reg(0x08, 64, 0);
        let irq = paula.tick_audio(2 * 227 + 20, dmacon, &ram);
        assert_eq!(irq & INT_AUD0, INT_AUD0);
        assert!(!paula.write_intreq(INT_AUD0));
        assert!(paula.chans[0].outputting());
        assert_eq!(paula.chans[0].audlen, 1);

        // The next fetch is the block's final word: its arrival reloads
        // the length counter and pointer from the latches and arms the
        // rollover interrupt, which fires at the following word start
        // (while the final word is still playing).
        let mut waited = 0;
        while !paula.chans[0].intreq2 {
            assert_eq!(paula.tick_audio(227, dmacon, &ram) & INT_AUD0, 0);
            waited += 1;
            assert!(waited <= 4, "final-word fetch should arrive");
        }
        assert_eq!(paula.chans[0].audlen, 2);
        assert_eq!(paula.chans[0].ptr, 0);
        let irq = paula.tick_audio(2 * 200, dmacon, &ram);
        assert_eq!(irq & INT_AUD0, INT_AUD0);
        assert!(!paula.chans[0].intreq2);
    }

    #[test]
    fn audio_dma_enable_during_manual_output_takes_over_at_word_start() {
        let (mut paula, _) = paula_with_collect_sink();
        let ram = vec![0u8; 64];
        let dmacon = DMACON_DMAEN | 0x0001;

        // Manual output running (acknowledged so the loop continues).
        paula.write_audio_reg(0x06, 8, 0);
        paula.write_audio_reg(0x0A, 0x1020, 0);
        assert!(!paula.write_intreq(INT_AUD0));
        paula.tick_audio(3, 0, &ram);
        assert_eq!(paula.chans[0].current, 0x10);

        // Enabling DMA mid-output does not restart the state machine (the
        // 000 -> 001 edge only fires from idle); instead the next word
        // start posts a DMA request and playback continues seamlessly.
        paula.write_audio_reg(0x00, 0, dmacon);
        paula.write_audio_reg(0x02, 0, dmacon);
        paula.write_audio_reg(0x04, 2, dmacon);
        paula.apply_audio_dmacon_edges(0, dmacon);
        assert!(paula.chans[0].outputting());
        assert!(!paula.chans[0].sm_dr);

        // The running word finishes on its own cadence...
        paula.tick_audio(5, dmacon, &ram);
        assert_eq!(paula.chans[0].current, 0x20);
        assert!(!paula.chans[0].sm_dr);
        // ...and the next word start posts the first DMA request (no
        // fresh start-up, no interrupt).
        assert_eq!(paula.tick_audio(8, dmacon, &ram) & INT_AUD0, 0);
        assert!(paula.chans[0].sm_dr, "word start should post a DMA request");
    }

    #[test]
    fn audio_dma_disable_reenabled_before_word_boundary_is_missed() {
        // Issue #74 (2c1.adf): Paula samples the AUDxEN clear only at the
        // word-start boundary (the 011 period event), never at the DMACON
        // write. A clear followed by a re-enable before the current DMA
        // word finishes is therefore invisible to the audio state machine:
        // no fresh start-up runs, the pointer/length are not reloaded from
        // AUDxLC/AUDxLEN, no start interrupt is raised, and playback simply
        // continues -- so the sample plays on (and loops) instead of
        // cleanly restarting. vAmiga idles immediately on the clear and
        // gets this case wrong.
        let (mut paula, _) = paula_with_collect_sink();
        let mut ram = vec![0u8; 64];
        ram[0] = 0x12;
        ram[1] = 0x34;
        ram[2] = 0x56;
        ram[3] = 0x78;
        let dmacon = DMACON_DMAEN | 0x0001;

        paula.write_audio_reg(0x00, 0, 0);
        paula.write_audio_reg(0x02, 0, 0);
        // A long buffer and a long period so the off/on pulse fits well
        // inside a single DMA word (no rollover in the window).
        paula.write_audio_reg(0x04, 4, 0);
        paula.write_audio_reg(0x06, 100, 0);
        let irq = paula.tick_audio(2 * 227 + 20, dmacon, &ram);
        assert_eq!(irq & INT_AUD0, INT_AUD0);
        assert!(!paula.write_intreq(INT_AUD0));
        assert!(paula.chans[0].outputting());
        let ptr_before = paula.chans[0].ptr;
        let audlen_before = paula.chans[0].audlen;

        // Clear AUDxEN: the channel keeps outputting the live word.
        assert_eq!(paula.tick_audio(1, 0, &ram) & INT_AUD0, 0);
        assert!(
            paula.chans[0].outputting(),
            "the clear is deferred, not sampled at the write"
        );
        // Re-enable before the word boundary: the clear is missed.
        assert_eq!(paula.tick_audio(1, dmacon, &ram) & INT_AUD0, 0);
        assert!(
            paula.chans[0].outputting(),
            "re-enable before the boundary continues the stream"
        );
        assert_eq!(
            paula.chans[0].ptr, ptr_before,
            "no restart: the DMA pointer is not reloaded from AUDxLC"
        );
        assert_eq!(paula.chans[0].audlen, audlen_before);

        // Playback continues in DMA mode across the following word boundary
        // with no fresh start interrupt.
        assert_eq!(paula.tick_audio(2 * 100, dmacon, &ram) & INT_AUD0, 0);
        assert!(paula.chans[0].outputting());
    }

    #[test]
    fn audio_dma_disable_left_off_idles_at_word_boundary_then_reenable_restarts() {
        let (mut paula, _) = paula_with_collect_sink();
        let mut ram = vec![0u8; 64];
        ram[0] = 0x12;
        ram[1] = 0x34;
        let dmacon = DMACON_DMAEN | 0x0001;

        paula.write_audio_reg(0x00, 0, 0);
        paula.write_audio_reg(0x02, 0, 0);
        paula.write_audio_reg(0x04, 2, 0);
        paula.write_audio_reg(0x06, 8, 0);
        let irq = paula.tick_audio(2 * 227 + 20, dmacon, &ram);
        assert_eq!(irq & INT_AUD0, INT_AUD0);
        assert!(!paula.write_intreq(INT_AUD0));
        assert!(paula.chans[0].outputting());

        // Clearing AUDxEN and leaving it off: the clear is deferred, so one
        // cck after the write the channel is still outputting the live word.
        assert_eq!(paula.tick_audio(1, 0, &ram) & INT_AUD0, 0);
        assert!(
            paula.chans[0].outputting(),
            "the clear is not sampled at the write"
        );

        // It idles at the word boundary where the (re-raised) channel
        // interrupt is left pending -- AUDxON low AND AUDxIP set.
        let mut waited = 1;
        while paula.chans[0].outputting() {
            paula.tick_audio(1, 0, &ram);
            waited += 1;
            assert!(waited <= 64, "held clear must idle at a word boundary");
        }
        assert_eq!(paula.chans[0].state, AUD_IDLE);
        // The DAC holds the last byte (Paula does not recentre the output).
        let held = paula.chans[0].current;
        paula.tick_audio(32, 0, &ram);
        assert_eq!(paula.chans[0].current, held);

        // Re-enabling from idle runs the full start-up again (fresh length
        // latch, start interrupt on the first fetch).
        let irq = paula.tick_audio(2 * 227 + 20, dmacon, &ram);
        assert_eq!(irq & INT_AUD0, INT_AUD0);
        assert!(paula.chans[0].outputting());
    }

    #[test]
    fn audio_channel_debug_snapshot_mirrors_live_state() {
        let (mut paula, _) = paula_with_collect_sink();
        let mut ram = vec![0u8; 64];
        ram[0] = 0x12;
        ram[1] = 0x34;
        let dmacon = DMACON_DMAEN | 0x0001;

        paula.write_audio_reg(0x00, 0, 0);
        paula.write_audio_reg(0x02, 0, 0);
        paula.write_audio_reg(0x04, 2, 0);
        paula.write_audio_reg(0x06, 8, 0);
        paula.write_audio_reg(0x08, 40, 0);
        paula.tick_audio(2 * 227 + 20, dmacon, &ram);

        let dbg = paula.audio_channel_debug(0).expect("channel 0 snapshot");
        assert!(dbg.playing);
        assert!(dbg.state.starts_with("01"), "output state: {}", dbg.state);
        assert_eq!(dbg.per, 8);
        assert_eq!(dbg.vol, 40);
        assert_eq!(dbg.audvol, 40);
        assert!(dbg.percnt <= 8);

        // Idling the channel shows in the snapshot; the DAC level holds.
        // Clearing AUDxEN is deferred to the word boundary, so tick past it
        // (the pending channel interrupt makes it idle there).
        let mut waited = 0;
        while paula.chans[0].outputting() {
            paula.tick_audio(1, 0, &ram);
            waited += 1;
            assert!(waited <= 64, "cleared channel must idle at a boundary");
        }
        let dbg = paula.audio_channel_debug(0).expect("channel 0 snapshot");
        assert!(!dbg.playing);
        assert_eq!(dbg.state, "000 idle");

        assert!(paula.audio_channel_debug(4).is_none());
    }

    #[test]
    fn audio_channel_mute_silences_mix_but_keeps_state() {
        let (mut paula, _) = paula_with_collect_sink();
        paula.write_audio_reg(0x06, 8, 0);
        paula.write_audio_reg(0x08, 40, 0);
        paula.write_audio_reg(0x0A, 0x4040, 0);

        // The channel contributes to the stereo mix.
        assert_ne!(paula.channel_mixed_sample(0), 0);
        let before = paula.audio_channel_debug(0).unwrap();

        // Muting zeroes the mix contribution but leaves the state machine,
        // volume, output sample and pointer untouched.
        paula.toggle_channel_muted(0);
        assert!(paula.channel_muted(0));
        assert_eq!(paula.channel_mixed_sample(0), 0);
        let after = paula.audio_channel_debug(0).unwrap();
        assert_eq!(after.state, before.state);
        assert_eq!(after.current, before.current);
        assert_eq!(after.vol, before.vol);
        assert_eq!(after.ptr, before.ptr);

        // Unmuting restores the contribution.
        paula.toggle_channel_muted(0);
        assert!(!paula.channel_muted(0));
        assert_ne!(paula.channel_mixed_sample(0), 0);
    }

    #[test]
    fn audio_scope_records_channel_output_level_even_when_muted() {
        let (mut paula, _) = paula_with_collect_sink();
        let mut ram = vec![0u8; 512 * 1024];
        for byte in &mut ram[0..4] {
            *byte = 0x40;
        }
        let dmacon = DMACON_DMAEN | 0x0001;
        paula.write_audio_reg(0x00, 0, 0);
        paula.write_audio_reg(0x02, 0, 0);
        paula.write_audio_reg(0x04, 2, 0);
        paula.write_audio_reg(0x06, 80, 0);
        paula.write_audio_reg(0x08, 64, 0);

        paula.tick_audio(4000, dmacon, &ram);
        let scope = paula.audio_scope_samples(0);
        assert!(!scope.is_empty());
        assert!(scope.len() <= AUDIO_SCOPE_LEN);
        assert!(
            scope.iter().any(|&s| s != 0),
            "scope should trace the channel output"
        );

        // The scope tap is pre-mute, so a muted channel keeps tracing.
        paula.toggle_channel_muted(0);
        assert!(paula.channel_muted(0));
        paula.tick_audio(4000, dmacon, &ram);
        assert!(paula.audio_scope_samples(0).iter().any(|&s| s != 0));
    }

    #[test]
    fn cd_audio_mute_zeroes_cd_contribution_but_scope_keeps_tracing() {
        let (mut paula, frames) = paula_with_collect_sink();
        paula.set_led_filter_guest(false);
        let ram = vec![0u8; 64];
        // One CD-DA sector (2352 bytes = 588 s16le stereo frames) of a
        // constant non-zero level.
        let mut sector = vec![0u8; 2352];
        for frame in sector.chunks_exact_mut(4) {
            frame[0..2].copy_from_slice(&4000i16.to_le_bytes());
            frame[2..4].copy_from_slice(&4000i16.to_le_bytes());
        }
        // No Paula channel audio, so the sink output is CD-only.
        let dmacon = DMACON_DMAEN;
        paula.cd_audio_mut().push_sector(&sector);
        paula.tick_audio(4000, dmacon, &ram);
        assert!(
            frames
                .borrow()
                .iter()
                .any(|&(l, r)| l.abs() > 1e-3 || r.abs() > 1e-3),
            "CD audio should reach the sink"
        );
        assert!(paula.cd_scope_samples().iter().any(|&s| s != 0));

        // Muted: the CD stream is silent in the mix, but the scope still
        // records the pre-mute level.
        frames.borrow_mut().clear();
        paula.toggle_cd_muted();
        assert!(paula.cd_muted());
        paula.cd_audio_mut().push_sector(&sector);
        paula.tick_audio(4000, dmacon, &ram);
        assert!(
            frames
                .borrow()
                .iter()
                .all(|&(l, r)| l.abs() < 1e-6 && r.abs() < 1e-6),
            "muted CD must be silent"
        );
        assert!(paula.cd_scope_samples().iter().any(|&s| s != 0));
    }

    #[test]
    fn toccata_and_mhi_mutes_zero_contribution_but_scopes_keep_tracing() {
        let (mut paula, frames) = paula_with_collect_sink();
        paula.set_led_filter_guest(false);
        let ram = vec![0u8; 64];
        // No Paula channel audio, so the sink output is board-only. The
        // boards' rings carry already-resampled mixer-rate frames.
        let dmacon = DMACON_DMAEN;
        for _ in 0..1024 {
            paula.toccata_audio_mut().push_frame(0.25, 0.25);
            paula.mhi_audio_mut().push_frame(-0.125, -0.125);
        }
        paula.tick_audio(4000, dmacon, &ram);
        assert!(
            frames
                .borrow()
                .iter()
                .any(|&(l, r)| l.abs() > 1e-3 || r.abs() > 1e-3),
            "board audio should reach the sink"
        );
        assert!(paula.toccata_scope_samples().iter().any(|&s| s != 0));
        assert!(paula.mhi_scope_samples().iter().any(|&s| s != 0));

        // Muted: both boards fall silent in the mix, but the scopes still
        // record the pre-mute levels.
        frames.borrow_mut().clear();
        paula.toggle_toccata_muted();
        paula.toggle_mhi_muted();
        assert!(paula.toccata_muted());
        assert!(paula.mhi_muted());
        for _ in 0..1024 {
            paula.toccata_audio_mut().push_frame(0.25, 0.25);
            paula.mhi_audio_mut().push_frame(-0.125, -0.125);
        }
        paula.tick_audio(4000, dmacon, &ram);
        assert!(
            frames
                .borrow()
                .iter()
                .all(|&(l, r)| l.abs() < 1e-6 && r.abs() < 1e-6),
            "muted boards must be silent"
        );
        assert!(paula.toccata_scope_samples().iter().any(|&s| s != 0));
        assert!(paula.mhi_scope_samples().iter().any(|&s| s != 0));
    }

    /// A serial sink standing in for an in-process synth: every mixer pull
    /// answers with a constant non-zero frame.
    struct ToneSynthSerial;

    impl SerialSink for ToneSynthSerial {
        fn write_byte(&mut self, _b: u8, _at_cck: u64) {}
        fn flush(&mut self) {}
        fn next_audio_frame(&mut self) -> Option<(f32, f32)> {
            Some((0.2, 0.2))
        }
    }

    #[test]
    fn synth_mute_zeroes_contribution_but_scope_keeps_tracing() {
        let frames = Rc::new(RefCell::new(Vec::new()));
        let audio = CollectSink {
            frames: Rc::clone(&frames),
        };
        let mut paula = Paula::new(Box::new(ToneSynthSerial), Box::new(audio));
        paula.set_led_filter_guest(false);
        let ram = vec![0u8; 64];
        let dmacon = DMACON_DMAEN;
        paula.tick_audio(4000, dmacon, &ram);
        assert!(
            frames
                .borrow()
                .iter()
                .any(|&(l, r)| l.abs() > 1e-3 || r.abs() > 1e-3),
            "synth audio should reach the sink"
        );
        assert!(paula.synth_scope_samples().iter().any(|&s| s != 0));

        // Muted: the synth is silent in the mix, but the scope still
        // records the pre-mute level.
        frames.borrow_mut().clear();
        paula.toggle_synth_muted();
        assert!(paula.synth_muted());
        paula.tick_audio(4000, dmacon, &ram);
        assert!(
            frames
                .borrow()
                .iter()
                .all(|&(l, r)| l.abs() < 1e-6 && r.abs() < 1e-6),
            "muted synth must be silent"
        );
        assert!(paula.synth_scope_samples().iter().any(|&s| s != 0));
    }

    #[test]
    fn audio_subminimum_period_replays_buffer_between_line_fetches() {
        // A channel gets one DMA slot per scanline. At periods far below
        // the sustainable floor the word is consumed long before the next
        // fetch arrives, so the output buffer reloads the same holding
        // register word and the samples repeat -- no starvation, no
        // spurious interrupts.
        let (mut paula, _) = paula_with_collect_sink();
        let mut ram = vec![0u8; 64];
        ram[0] = 0x10;
        ram[1] = 0x20;
        ram[2] = 0x30;
        ram[3] = 0x40;
        let dmacon = DMACON_DMAEN | 0x0001;

        paula.write_audio_reg(0x00, 0, 0);
        paula.write_audio_reg(0x02, 0, 0);
        paula.write_audio_reg(0x04, 8, 0);
        paula.write_audio_reg(0x06, 2, 0);
        paula.write_audio_reg(0x08, 64, 0);

        let irq = paula.tick_audio(2 * 227 + 20, dmacon, &ram);
        assert_eq!(irq & INT_AUD0, INT_AUD0);
        assert!(paula.chans[0].outputting());

        // Between fetches the same word keeps replaying.
        let word = paula.chans[0].buffer;
        for _ in 0..20 {
            paula.tick_audio(2, dmacon, &ram);
            assert_eq!(paula.chans[0].buffer, word);
        }
    }

    #[test]
    fn audio_period_zero_counts_a_full_wrap_without_irq_storm() {
        let (mut paula, _) = paula_with_collect_sink();
        let ram = vec![0u8; 64];

        // AUDxPER=0 counts 65536 clocks per boundary (the counter wraps),
        // so the sample holds for a very long time without interrupting.
        paula.write_audio_reg(0x06, 0, 0);
        paula.write_audio_reg(0x0A, 0x5566, 0);
        assert_eq!(paula.chans[0].current, 0x55);
        assert!(!paula.write_intreq(INT_AUD0));

        assert_eq!(paula.tick_audio(4000, 0, &ram) & INT_AUD0, 0);
        assert_eq!(paula.chans[0].current, 0x55);
        assert_eq!(paula.chans[0].state, AUD_OUT_HI);
    }

    #[test]
    fn audio_period_write_latches_without_disturbing_live_countdown() {
        let (mut paula, _) = paula_with_collect_sink();
        let ram = vec![0u8; 64];

        paula.write_audio_reg(0x06, 10, 0);
        paula.write_audio_reg(0x0A, 0x1234, 0);
        assert!(!paula.write_intreq(INT_AUD0));
        paula.tick_audio(5, 0, &ram);
        assert_eq!(paula.chans[0].percnt, 5);

        // A mid-count AUDxPER write only updates the latch; the running
        // boundary completes on the old cadence and the next one reloads
        // the new period.
        paula.write_audio_reg(0x06, 7, 0);
        assert_eq!(paula.chans[0].percnt, 5);
        paula.tick_audio(5, 0, &ram);
        assert_eq!(paula.chans[0].current, 0x34);
        assert_eq!(paula.chans[0].percnt, 7);
    }

    #[test]
    fn cpu_auddat_write_during_dma_feeds_the_state_machine_like_a_fetch() {
        // AUDxDAT is the state machine's input register: Paula cannot tell
        // a CPU write from the DMA slot's write, so a CPU poke during DMA
        // playback counts against the length counter exactly like a fetch.
        let (mut paula, _) = paula_with_collect_sink();
        let mut ram = vec![0u8; 64];
        ram[0] = 0x11;
        ram[1] = 0x22;
        let dmacon = DMACON_DMAEN | 0x0001;

        paula.write_audio_reg(0x00, 0, 0);
        paula.write_audio_reg(0x02, 0, 0);
        paula.write_audio_reg(0x04, 8, 0);
        paula.write_audio_reg(0x06, 100, 0);
        paula.write_audio_reg(0x08, 64, 0);
        let irq = paula.tick_audio(2 * 227 + 20, dmacon, &ram);
        assert_eq!(irq & INT_AUD0, INT_AUD0);
        assert!(paula.chans[0].outputting());
        let len_before = paula.chans[0].audlen;

        paula.write_audio_reg(0x0A, 0x7F7E, dmacon);
        assert_eq!(paula.chans[0].audlen, len_before.wrapping_sub(1));
        assert_eq!(paula.peek_audio_reg_latch(0x0A), Some(0x7F7E));
        // The poked word becomes the next output word at the word start.
        let mut waited = 0;
        while paula.chans[0].buffer != 0x7F7E {
            paula.tick_audio(100, dmacon, &ram);
            waited += 1;
            assert!(waited <= 4, "poked word should reach the buffer");
        }
    }

    #[test]
    fn auddat_latch_does_not_restart_output_after_dma_disable() {
        let (mut paula, _) = paula_with_collect_sink();
        let mut ram = vec![0u8; 64];
        ram[0] = 0x12;
        ram[1] = 0x34;
        let dmacon = DMACON_DMAEN | 0x0001;

        paula.write_audio_reg(0x00, 0, 0);
        paula.write_audio_reg(0x02, 0, 0);
        paula.write_audio_reg(0x04, 1, 0);
        paula.write_audio_reg(0x06, 8, 0);
        let irq = paula.tick_audio(2 * 227 + 20, dmacon, &ram);
        assert_eq!(irq & INT_AUD0, INT_AUD0);

        // Disable: the channel idles; the stale AUDxDAT holding register
        // must not spontaneously start IRQ-mode output.
        paula.tick_audio(14, 0, &ram);
        assert_eq!(paula.chans[0].state, AUD_IDLE);
        let held = paula.chans[0].current;
        paula.tick_audio(64, 0, &ram);
        assert_eq!(paula.chans[0].state, AUD_IDLE);
        assert_eq!(paula.chans[0].current, held);
    }

    #[test]
    fn wav_capture_records_deterministic_paula_dma_window() {
        let path =
            std::env::temp_dir().join(format!("copperline-paula-dma-window-{}.wav", process::id()));
        let _ = std::fs::remove_file(&path);

        {
            let wav = WavSink::new(&path).expect("create wav sink");
            let mut paula = Paula::new(Box::new(NoopSerial), Box::new(wav));
            paula.set_led_filter_guest(false);
            let mut ram = vec![0u8; 64];
            ram[0] = 0x40;
            ram[1] = 0xC0;
            ram[2] = 0x40;
            ram[3] = 0xC0;

            paula.write_audio_reg(0x00, 0, 0);
            paula.write_audio_reg(0x02, 0, 0);
            paula.write_audio_reg(0x04, 2, 0);
            paula.write_audio_reg(0x06, 400, 0);
            paula.write_audio_reg(0x08, 64, 0);
            let _ = paula.tick_audio(4_000, DMACON_DMAEN | 0x0001, &ram);
        }

        let mut reader = hound::WavReader::open(&path).expect("open wav");
        let spec = reader.spec();
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, MIX_SAMPLE_RATE);
        assert_eq!(spec.bits_per_sample, 32);
        assert_eq!(spec.sample_format, hound::SampleFormat::Float);

        let samples = reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .expect("read wav samples");
        let lefts: Vec<f32> = samples.chunks_exact(2).map(|frame| frame[0]).collect();
        let rights: Vec<f32> = samples.chunks_exact(2).map(|frame| frame[1]).collect();
        // Silent lead-in while the start-up fetches run, then the sample
        // alternates +-0.5 at the period cadence. Left channel only.
        assert_eq!(lefts[0], 0.0);
        assert!(lefts.iter().any(|&l| (l - 0.5).abs() < f32::EPSILON));
        assert!(lefts.iter().any(|&l| (l + 0.5).abs() < f32::EPSILON));
        assert!(rights.iter().all(|&r| r == 0.0));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wav_capture_routes_paula_channels_to_stereo_pairs() {
        let path = std::env::temp_dir().join(format!(
            "copperline-paula-channel-routing-{}.wav",
            process::id()
        ));
        let _ = std::fs::remove_file(&path);

        {
            let wav = WavSink::new(&path).expect("create wav sink");
            let mut paula = Paula::new(Box::new(NoopSerial), Box::new(wav));
            paula.set_led_filter_guest(false);

            for ch_idx in 0..4 {
                paula.chans[ch_idx].current = 64;
                paula.chans[ch_idx].audvol = 64;
                paula.push_mixed_frame();
                paula.chans[ch_idx].current = 0;
            }
        }

        let mut reader = hound::WavReader::open(&path).expect("open wav");
        let spec = reader.spec();
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, MIX_SAMPLE_RATE);
        assert_eq!(spec.bits_per_sample, 32);
        assert_eq!(spec.sample_format, hound::SampleFormat::Float);

        let samples = reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .expect("read wav samples");
        let frames = samples
            .chunks_exact(2)
            .map(|frame| (frame[0], frame[1]))
            .collect::<Vec<_>>();

        assert_eq!(frames, &[(0.5, 0.0), (0.0, 0.5), (0.0, 0.5), (0.5, 0.0)]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn serdat_uses_timed_transmit_shift_register() {
        let (mut paula, written, _) = paula_with_collect_serial();

        assert_eq!(paula.write_serdat(0x0141), INT_TBE);
        assert_eq!(paula.next_serial_event_cck(), Some(1));
        assert!(!paula.serial_txd_pin_high());
        assert_ne!(paula.read_serdatr() & (1 << 13), 0);
        assert_eq!(paula.read_serdatr() & (1 << 12), 0);
        assert!(written.lock().unwrap().is_empty());

        assert_eq!(paula.tick_serial(1, 0), 0);
        assert!(paula.serial_txd_pin_high());
        assert_eq!(paula.tick_serial(8, 0), 0);
        assert_eq!(paula.next_serial_event_cck(), Some(1));
        assert!(paula.serial_txd_pin_high());
        assert!(written.lock().unwrap().is_empty());
        assert_eq!(paula.tick_serial(1, 0), 0);
        assert_eq!(paula.next_serial_event_cck(), None);
        assert_eq!(&*written.lock().unwrap(), &[0x41]);
        assert_ne!(paula.read_serdatr() & (1 << 12), 0);
    }

    #[test]
    fn serial_tx_stamps_byte_with_emit_color_clock() {
        // Transmit one byte in a single span and read back the emit-time stamp.
        // Two spans ending 5000 clocks apart must stamp the byte 5000 clocks
        // apart, without hard-coding the framing length.
        fn transmit(end_cck: u64) -> (u16, u64) {
            let events = Arc::new(Mutex::new(Vec::new()));
            let mut paula = Paula::new(
                Box::new(TimedSerial {
                    events: Arc::clone(&events),
                }),
                Box::new(NullAudio),
            );
            assert_eq!(paula.write_serdat(0x0141), INT_TBE);
            // A span longer than the framing completes the byte in one call.
            paula.tick_serial(100, end_cck);
            let ev = events.lock().unwrap();
            assert_eq!(ev.len(), 1, "exactly one byte should be emitted");
            ev[0]
        }

        let (word0, at0) = transmit(1000);
        let (word_b, at_b) = transmit(6000);
        assert_eq!(word0, 0x41);
        assert_eq!(word_b, 0x41);
        assert!(at0 > 0, "emit time should be before the span end");
        assert_eq!(at_b, at0 + 5000, "the span end offsets the emit time");
    }

    #[test]
    fn serial_observer_taps_completed_words_without_replacing_the_sink() {
        let (mut paula, written, _) = paula_with_collect_serial_words();
        paula.set_serial_observation_enabled(true);
        assert_eq!(paula.write_serdat(0x0141), INT_TBE);
        assert_eq!(paula.tick_serial(10, 100), 0);

        assert_eq!(&*written.lock().unwrap(), &[(0x0041, false)]);
        let (observations, dropped) = paula.take_serial_observations();
        assert_eq!(dropped, 0);
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].word, 0x0041);
        assert!(!observations[0].long);
        assert_eq!(observations[0].at_cck, 100);
    }

    #[test]
    fn pending_sink_input_bounds_the_serial_event_horizon() {
        // An idle receiver with host bytes queued must report an imminent
        // serial event: real Paula starts shifting when the start bit hits
        // the pin, so the bus needs a span boundary now, not at the end of
        // a long idle chunk. Regression: without this, a burst arriving on
        // an idle machine loaded and completed several words inside one
        // span -- the first buffered, the rest overran -- before the CPU
        // ever saw RBF.
        let (mut paula, _, _) = paula_with_collect_serial_words();
        paula.serper = 30;
        assert_eq!(paula.next_serial_event_cck(), None);

        let (sink, handle) = crate::serial::ChannelSerialSink::pair();
        paula.serial = Box::new(sink);
        handle.push_input(b"ab");
        assert_eq!(paula.next_serial_event_cck(), Some(1));

        // Ticking loads the shift; the horizon then tracks it bit by bit,
        // and the queued second byte keeps the horizon short after the
        // first completes instead of letting it go idle-long again.
        assert_eq!(paula.tick_serial(1, 1), 0);
        let horizon = paula.next_serial_event_cck().expect("shift active");
        assert!(horizon <= 31, "horizon {horizon} exceeds a bit time");
    }

    #[test]
    fn serdat_masks_stop_bit_and_preserves_long_data_bit_for_word_sinks() {
        let (mut paula, written, _) = paula_with_collect_serial_words();

        assert_eq!(paula.write_serdat(0x0141), INT_TBE);
        assert_eq!(paula.tick_serial(10, 0), 0);
        assert_eq!(&*written.lock().unwrap(), &[(0x0041, false)]);

        paula.serper = SERPER_LONG;
        assert_eq!(paula.write_serdat(0x0342), INT_TBE);
        assert_eq!(paula.tick_serial(10, 0), 0);
        assert_eq!(&*written.lock().unwrap(), &[(0x0041, false)]);
        assert_eq!(paula.tick_serial(1, 0), 0);
        assert_eq!(
            &*written.lock().unwrap(),
            &[(0x0041, false), (0x0142, true)]
        );
    }

    #[test]
    fn serper_long_extends_serial_word_timing() {
        let (mut paula, written, _) = paula_with_collect_serial();
        paula.serper = SERPER_LONG;

        assert_eq!(paula.write_serdat(0x0341), INT_TBE);
        assert_eq!(paula.tick_serial(10, 0), 0);
        assert!(written.lock().unwrap().is_empty());
        assert_eq!(paula.tick_serial(1, 0), 0);
        assert_eq!(&*written.lock().unwrap(), &[0x41]);
    }

    #[test]
    fn adkcon_uartbrk_forces_serial_txd_low() {
        let (mut paula, written, _) = paula_with_collect_serial();

        assert_eq!(paula.write_serdat(0x01FF), INT_TBE);
        assert!(!paula.serial_txd_pin_high());
        assert_eq!(paula.tick_serial(1, 0), 0);
        assert!(paula.serial_txd_pin_high());

        paula.write_adkcon(0x8000 | ADKCON_UARTBRK);
        assert!(!paula.serial_txd_pin_high());
        assert_eq!(paula.tick_serial(9, 0), 0);
        assert!(written.lock().unwrap().is_empty());

        paula.write_adkcon(ADKCON_UARTBRK);
        assert!(paula.serial_txd_pin_high());
    }

    #[test]
    fn serdatr_reports_receive_buffer_and_overrun_until_rbf_clear() {
        let (mut paula, _, read) = paula_with_collect_serial();
        read.lock().unwrap().extend_from_slice(&[0x55, 0x66]);

        assert_eq!(paula.tick_serial(9, 0) & INT_RBF, 0);
        let irq = paula.tick_serial(1, 0);
        assert_eq!(irq & INT_RBF, INT_RBF);
        paula.latch_interrupt_sources(irq);
        let serdatr = paula.read_serdatr();
        assert_ne!(serdatr & (1 << 14), 0);
        assert_eq!(serdatr & (1 << 15), 0);
        assert_eq!(serdatr & 0x00FF, 0x55);
        assert_eq!(serdatr & 0x0300, 0x0300);

        assert_eq!(paula.tick_serial(10, 0) & INT_RBF, 0);
        assert_ne!(paula.read_serdatr() & (1 << 15), 0);

        paula.write_intreq(INT_RBF);
        assert_eq!(paula.read_serdatr() & ((1 << 15) | (1 << 14)), 0);
    }

    #[test]
    fn rbf_ack_leaves_received_word_latched_in_serdatr() {
        // AROS's level-5 dispatcher acks INTREQ BEFORE running the RBF
        // handler, which then reads the word from SERDATR. The receive
        // buffer is a physical latch: the ack drops RBF and OVRUN but must
        // leave the data readable, and with RBF clear the next word stores
        // instead of overrunning.
        let (mut paula, _, read) = paula_with_collect_serial();
        read.lock().unwrap().extend_from_slice(&[0x41, 0x42]);

        assert_eq!(paula.tick_serial(9, 0) & INT_RBF, 0);
        let irq = paula.tick_serial(1, 0);
        assert_eq!(irq & INT_RBF, INT_RBF);
        paula.latch_interrupt_sources(irq);

        paula.write_intreq(INT_RBF); // the AROS-style early ack
        let serdatr = paula.read_serdatr();
        assert_eq!(serdatr & (1 << 14), 0, "RBF drops with the ack");
        assert_eq!(serdatr & 0x00FF, 0x41, "data survives the ack");

        let irq = paula.tick_serial(10, 0);
        assert_eq!(irq & INT_RBF, INT_RBF, "next word stores, no overrun");
        paula.latch_interrupt_sources(irq);
        let serdatr = paula.read_serdatr();
        assert_eq!(serdatr & 0x00FF, 0x42);
        assert_eq!(serdatr & (1 << 15), 0);
    }

    #[test]
    fn serper_long_receive_keeps_ninth_data_bit() {
        let (mut paula, _, read) = paula_with_collect_serial_words();
        paula.serper = SERPER_LONG;
        read.lock().unwrap().push(0x0155);

        assert_eq!(paula.tick_serial(10, 0) & INT_RBF, 0);
        let irq = paula.tick_serial(1, 0);
        assert_eq!(irq & INT_RBF, INT_RBF);
        paula.latch_interrupt_sources(irq);
        let serdatr = paula.read_serdatr();
        assert_ne!(serdatr & (1 << 14), 0);
        assert_eq!(serdatr & 0x03FF, 0x0355);
    }

    #[test]
    fn serdatr_rxd_uses_two_stage_synchronized_pin() {
        let (mut paula, _, read) = paula_with_collect_serial();
        paula.serper = 3;
        read.lock().unwrap().push(0x01);

        assert_eq!(paula.tick_serial(0, 0), 0);
        assert_ne!(paula.read_serdatr() & (1 << 11), 0);
        assert_eq!(paula.tick_serial(1, 0), 0);
        assert_ne!(paula.read_serdatr() & (1 << 11), 0);
        assert_eq!(paula.tick_serial(1, 0), 0);
        assert_eq!(paula.read_serdatr() & (1 << 11), 0);

        assert_eq!(paula.tick_serial(2, 0), 0);
        assert_eq!(paula.read_serdatr() & (1 << 11), 0);
        assert_eq!(paula.tick_serial(1, 0), 0);
        assert_eq!(paula.read_serdatr() & (1 << 11), 0);
        assert_eq!(paula.tick_serial(1, 0), 0);
        assert_ne!(paula.read_serdatr() & (1 << 11), 0);
    }

    #[test]
    fn adkcon_both_attach_applies_period_mid_word_and_volume_at_word_start() {
        // With AUDxAP and AUDxAV both set the channel consumes its words
        // as modulation data: the 010 -> 011 transition writes the period
        // latch (pbufld2) and the word start writes the volume latch
        // (pbufld1); no alternation flag is involved.
        let (mut paula, _) = paula_with_collect_sink();
        let ram = vec![0u8; 64];
        let dmacon = DMACON_DMAEN | 0x0001;

        paula.chans[0].state = AUD_OUT_HI;
        paula.chans[0].per = 2;
        paula.chans[0].percnt = 1;
        paula.chans[0].audlen = 8;
        paula.chans[0].auddat = 0x0030;
        paula.chans[1].vol = 1;
        paula.chans[1].per = 100;
        paula.write_adkcon(0x8000 | 0x0011);

        paula.tick_audio(1, dmacon, &ram);
        assert_eq!(paula.chans[0].state, AUD_OUT_LO);
        assert_eq!(paula.chans[1].per, 0x30);
        assert_eq!(paula.chans[1].vol, 1);

        paula.tick_audio(2, dmacon, &ram);
        assert_eq!(paula.chans[0].state, AUD_OUT_HI);
        assert_eq!(paula.chans[1].vol, 0x30);
    }

    #[test]
    fn adkcon_volume_modulation_can_enable_and_disable_mid_stream() {
        let (mut paula, _) = paula_with_collect_sink();
        let ram = vec![0u8; 64];
        let dmacon = DMACON_DMAEN | 0x0001;

        paula.chans[0].state = AUD_OUT_LO;
        paula.chans[0].per = 4;
        paula.chans[0].percnt = 4;
        paula.chans[0].audlen = 8;
        paula.chans[0].auddat = 0x0034;
        paula.chans[1].vol = 1;

        // Attach off: the word start feeds the channel's own buffer.
        assert_eq!(paula.tick_audio(4, dmacon, &ram) & INT_AUD0, 0);
        assert_eq!(paula.chans[1].vol, 1);

        // Attach on: the next word start drives channel 1's volume latch.
        paula.write_adkcon(0x8000 | 0x0001);
        assert_eq!(paula.tick_audio(8, dmacon, &ram) & INT_AUD0, 0);
        assert_eq!(paula.chans[1].vol, 0x34);

        // Attach off again: the latch keeps its value.
        paula.write_adkcon(0x0001);
        paula.chans[0].auddat = 0x0011;
        assert_eq!(paula.tick_audio(8, dmacon, &ram) & INT_AUD0, 0);
        assert_eq!(paula.chans[1].vol, 0x34);
    }

    #[test]
    fn adkcon_volume_modulation_uses_low_seven_bits_of_word() {
        let (mut paula, _) = paula_with_collect_sink();
        let ram = vec![0u8; 64];
        let dmacon = DMACON_DMAEN | 0x0001;

        paula.chans[0].state = AUD_OUT_LO;
        paula.chans[0].per = 4;
        paula.chans[0].percnt = 4;
        paula.chans[0].audlen = 8;
        paula.chans[0].auddat = 0xFF3F;
        paula.chans[1].vol = 1;
        paula.write_adkcon(0x8000 | 0x0001);

        assert_eq!(paula.tick_audio(4, dmacon, &ram) & INT_AUD0, 0);
        assert_eq!(paula.chans[1].vol, 0x3F);

        // Values above 64 in the low seven bits clamp to full volume.
        paula.chans[0].auddat = 0x807F;
        assert_eq!(paula.tick_audio(8, dmacon, &ram) & INT_AUD0, 0);
        assert_eq!(paula.chans[1].vol, 64);
    }

    #[test]
    fn adkcon_period_modulation_writes_target_period_latch() {
        let (mut paula, _) = paula_with_collect_sink();
        let ram = vec![0u8; 64];
        let dmacon = DMACON_DMAEN | 0x0004;

        paula.chans[2].state = AUD_OUT_HI;
        paula.chans[2].per = 4;
        paula.chans[2].percnt = 4;
        paula.chans[2].audlen = 8;
        paula.chans[2].auddat = 0x0002;
        paula.chans[3].per = 100;
        paula.write_adkcon(0x8000 | 0x0040);

        // The 010 -> 011 transition writes channel 3's period latch, tiny
        // values included (Paula does not clamp the latch itself).
        assert_eq!(paula.tick_audio(4, dmacon, &ram) & INT_AUD2, 0);
        assert_eq!(paula.chans[3].per, 2);
    }

    #[test]
    fn adkcon_attach_period_moves_requests_and_irq_to_mid_word() {
        // In attach-period mode (napnav false) the word-start transition
        // posts no DMA request and no interrupt; both move to the
        // 010 -> 011 transition.
        let (mut paula, _) = paula_with_collect_sink();
        let ram = vec![0u8; 64];
        let dmacon = DMACON_DMAEN | 0x0001;

        paula.chans[0].state = AUD_OUT_LO;
        paula.chans[0].per = 4;
        paula.chans[0].percnt = 4;
        paula.chans[0].audlen = 8;
        paula.chans[0].auddat = 0x0050;
        paula.write_adkcon(0x8000 | 0x0010);

        // Word start: no request, no interrupt.
        assert_eq!(paula.tick_audio(4, dmacon, &ram) & INT_AUD0, 0);
        assert_eq!(paula.chans[0].state, AUD_OUT_HI);
        assert!(!paula.chans[0].sm_dr);

        // Mid-word: the period latch write, the DMA request, and (with
        // intreq2 armed) the rollover interrupt all happen here.
        paula.chans[0].intreq2 = true;
        let irq = paula.tick_audio(4, dmacon, &ram);
        assert_eq!(irq & INT_AUD0, INT_AUD0);
        assert_eq!(paula.chans[0].state, AUD_OUT_LO);
        assert!(paula.chans[0].sm_dr);
        assert_eq!(paula.chans[1].per, 0x50);
        assert!(!paula.chans[0].intreq2);
    }

    #[test]
    fn adkcon_attached_source_channel_is_not_mixed_to_dac() {
        let (mut paula, frames) = paula_with_collect_sink();
        paula.set_led_filter_guest(false);
        paula.chans[0].current = 127;
        paula.chans[0].audvol = 64;
        paula.chans[1].current = 127;
        paula.chans[1].audvol = 64;

        paula.write_adkcon(0x8000 | 0x0001);
        paula.advance_audio(PAULA_CLOCK_HZ.div_ceil(MIX_SAMPLE_RATE), 0);

        let frames = frames.borrow();
        let (left, right) = frames[0];
        assert_eq!(left, 0.0);
        assert!(right > 0.9, "target channel should remain audible: {right}");
    }

    #[test]
    fn led_filter_attenuates_high_frequency_output() {
        fn alternating_average(filter_enabled: bool) -> f32 {
            let (mut paula, frames) = paula_with_collect_sink();
            paula.set_led_filter_guest(filter_enabled);
            paula.chans[0].audvol = 64;
            for i in 0..256 {
                paula.chans[0].current = if i & 1 == 0 { 127 } else { -127 };
                paula.push_mixed_frame();
            }
            let frames = frames.borrow();
            let settled = &frames[64..];
            settled.iter().map(|(left, _)| left.abs()).sum::<f32>() / settled.len() as f32
        }

        let bypassed = alternating_average(false);
        let filtered = alternating_average(true);
        assert!(
            filtered < bypassed * 0.25,
            "LED filter should attenuate high-frequency alternation, bypassed={bypassed}, filtered={filtered}"
        );
    }

    #[test]
    fn led_filter_has_three_pole_four_kilohertz_shape() {
        fn gain_at(freq_hz: f32) -> f32 {
            let mut filter = AnalogLedFilter::new(LED_FILTER_CUTOFF_HZ, MIX_SAMPLE_RATE as f32);
            let sample_rate = MIX_SAMPLE_RATE as f32;
            let settle = 4096;
            let samples = MIX_SAMPLE_RATE as usize;
            let mut in_sq = 0.0;
            let mut out_sq = 0.0;
            for n in 0..samples {
                let phase = 2.0 * PI * freq_hz * n as f32 / sample_rate;
                let input = phase.sin();
                let output = filter.process(input);
                if n >= settle {
                    in_sq += input * input;
                    out_sq += output * output;
                }
            }
            (out_sq / in_sq).sqrt()
        }

        let low = gain_at(1_000.0);
        let knee = gain_at(LED_FILTER_CUTOFF_HZ);
        let high = gain_at(12_000.0);

        assert!(low > 0.75, "1 kHz should stay in passband: {low}");
        assert!(
            (0.35..0.70).contains(&knee),
            "4 kHz should be near the combined one-pole/two-pole knee: {knee}"
        );
        assert!(
            high < 0.15,
            "12 kHz should be strongly attenuated by the three-pole cascade: {high}"
        );
        assert!(low > knee * 1.5 && knee > high * 3.0);
    }

    #[test]
    fn wav_capture_led_filter_records_bypassed_and_filtered_levels() {
        fn alternating_wav_average(filter_enabled: bool, label: &str) -> f32 {
            let path = std::env::temp_dir().join(format!(
                "copperline-paula-led-filter-{label}-{}.wav",
                process::id()
            ));
            let _ = std::fs::remove_file(&path);

            {
                let wav = WavSink::new(&path).expect("create wav sink");
                let mut paula = Paula::new(Box::new(NoopSerial), Box::new(wav));
                paula.set_led_filter_guest(filter_enabled);
                paula.chans[0].audvol = 64;
                for i in 0..256 {
                    paula.chans[0].current = if i & 1 == 0 { 127 } else { -127 };
                    paula.push_mixed_frame();
                }
            }

            let mut reader = hound::WavReader::open(&path).expect("open wav");
            let spec = reader.spec();
            assert_eq!(spec.channels, 2);
            assert_eq!(spec.sample_rate, MIX_SAMPLE_RATE);
            let samples = reader
                .samples::<f32>()
                .collect::<Result<Vec<_>, _>>()
                .expect("read wav samples");
            let frames = samples.chunks_exact(2).collect::<Vec<_>>();
            let average = frames[64..].iter().map(|frame| frame[0].abs()).sum::<f32>()
                / (frames.len() - 64) as f32;

            let _ = std::fs::remove_file(&path);
            average
        }

        let bypassed = alternating_wav_average(false, "bypassed");
        let filtered = alternating_wav_average(true, "filtered");
        assert!(
            filtered < bypassed * 0.20,
            "WAV-level LED filter should attenuate alternating output, bypassed={bypassed}, filtered={filtered}"
        );
    }

    #[test]
    fn host_output_volume_scales_mixed_audio_without_changing_audvol() {
        let (mut paula, frames) = paula_with_collect_sink();
        paula.set_led_filter_guest(false);
        paula.set_output_volume_percent(50);
        paula.chans[0].current = 64;
        paula.chans[0].audvol = 64;

        paula.push_mixed_frame();

        let frames = frames.borrow();
        assert_eq!(paula.output_volume_percent(), 50);
        assert_eq!(paula.chans[0].audvol, 64);
        assert!((frames[0].0 - 0.25).abs() < f32::EPSILON);
        assert_eq!(frames[0].1, 0.0);
    }

    #[test]
    fn mono_output_averages_left_and_right_into_both_channels() {
        let (mut paula, frames) = paula_with_collect_sink();
        paula.set_led_filter_guest(false);
        paula.set_mono_output(true);
        // Drive only a left channel (0); the right side stays silent, so stereo
        // output would be (0.5, 0.0) and mono is the average, 0.25, in both.
        paula.chans[0].current = 64;
        paula.chans[0].audvol = 64;

        paula.push_mixed_frame();

        let frames = frames.borrow();
        assert_eq!(frames[0].0, frames[0].1, "mono means identical channels");
        assert!((frames[0].0 - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn stereo_separation_narrows_from_hardware_panning_toward_mono() {
        // Drive left channel only: hardware panning gives (0.5, 0.0).
        let out = |sep: f32| {
            let (mut paula, frames) = paula_with_collect_sink();
            paula.set_led_filter_guest(false);
            paula.set_stereo_separation(sep);
            paula.chans[0].current = 64;
            paula.chans[0].audvol = 64;
            paula.push_mixed_frame();
            let frame = frames.borrow()[0];
            frame
        };
        // 100%: untouched.
        let full = out(1.0);
        assert!((full.0 - 0.5).abs() < f32::EPSILON && full.1 == 0.0);
        // 0%: mono (both = the 0.25 average).
        let mono = out(0.0);
        assert_eq!(mono.0, mono.1);
        assert!((mono.0 - 0.25).abs() < f32::EPSILON);
        // 50%: mid 0.25 +/- side (0.25 * 0.5) -> (0.375, 0.125).
        let half = out(0.5);
        assert!((half.0 - 0.375).abs() < f32::EPSILON);
        assert!((half.1 - 0.125).abs() < f32::EPSILON);
    }

    #[test]
    fn pot_hsync_counter_ramps_once_per_line() {
        let (mut paula, _) = paula_with_collect_sink();

        assert!(!paula.pot_running());
        paula.write_potgo(0x0001);
        assert!(paula.pot_running());
        assert_eq!(paula.read_potdat(0), 0x0000);

        // Each H-sync advances both bytes of POT0DAT (the X and Y pin counters)
        // by one.
        for _ in 0..5 {
            paula.tick_pot_hsync(PotPins::default(), 0);
        }
        assert_eq!(paula.read_potdat(0), 0x0505);

        // A floating pin wraps at 256 rather than saturating, so the scan keeps
        // running indefinitely.
        paula.pot_counters = [u8::MAX; 4];
        paula.tick_pot_hsync(PotPins::default(), 0);
        assert_eq!(paula.read_potdat(0), 0x0000);
        assert!(paula.pot_running());
    }

    #[test]
    fn pot_pin_driven_high_holds_counter_at_zero() {
        // A pot pin driven HIGH as an output (OUTxx + DATxx set) charges its
        // cap through the low-impedance driver, so the comparator trips at once
        // and the counter stays at its START-reset value of 0. Software relies
        // on this to read POTxDAT back as ~0 after POTGO=$FFFF (the Bitmap
        // Brothers input code keys a "no second button" test on POT0DAT, and a
        // spuriously counting pin sets a phantom button that breaks controls).
        let charge_lines = 0x20;

        // Drive every pin high: both POTxDAT words read back as zero.
        let (mut paula, _) = paula_with_collect_sink();
        paula.write_potgo(0xFFFF);
        for _ in 0..charge_lines {
            paula.tick_pot_hsync(PotPins::default(), 8);
        }
        assert_eq!(paula.read_potdat(0), 0x0000);
        assert_eq!(paula.read_potdat(1), 0x0000);
        // ...and the scan terminates rather than ticking forever.
        assert!(!paula.pot_running());

        // Per-pin: driving only the port-0 pins high (DATLX/OUTLX/DATLY/OUTLY =
        // bits 8..11) holds POT0DAT at zero while the floating port-1 pins still
        // charge up, proving the gate is per counter and not global.
        let (mut paula, _) = paula_with_collect_sink();
        paula.write_potgo(0x0F00 | 0x0001);
        for _ in 0..charge_lines {
            paula.tick_pot_hsync(PotPins::default(), 8);
        }
        assert_eq!(paula.read_potdat(0), 0x0000);
        assert_ne!(paula.read_potdat(1), 0x0000);
    }

    #[test]
    fn potgor_reports_the_production_paula_id_field() {
        let (mut paula, _) = paula_with_collect_sink();
        paula.write_potgo(0x00FF);
        assert_eq!(
            paula.read_potgor(PotPins::default()) & 0x00FE,
            POTGOR_PAULA_ID
        );
    }

    #[test]
    fn pot_rc_scan_holds_reset_then_latches_from_resistance() {
        let (mut paula, _) = paula_with_collect_sink();
        let pins = PotPins {
            resistance_ohms: [Some(0), Some(470_000), Some(264_000), None],
            ..PotPins::default()
        };
        paula.write_potgo(0x0001);

        for _ in 0..8 {
            paula.tick_pot_hsync(pins, 8);
            assert_eq!(paula.read_potdat(0), 0, "PAL discharge holds reset");
        }

        // Zero ohms crosses immediately at count zero. Half-scale resistance
        // crosses at 128; the recommended 470 kOhm controller crosses at 227.
        paula.tick_pot_hsync(pins, 8);
        assert_eq!(paula.read_potdat(0) & 0x00FF, 0);
        for _ in 1..128 {
            paula.tick_pot_hsync(pins, 8);
        }
        assert_eq!(paula.read_potdat(1) & 0x00FF, 128);
        for _ in 128..227 {
            paula.tick_pot_hsync(pins, 8);
        }
        assert_eq!(paula.read_potdat(0) >> 8, 227);
        assert!(paula.pot_running(), "the disconnected POT1Y keeps scanning");
    }

    #[test]
    fn pot_position_resistance_round_trips_every_count() {
        // The analogue-controller position converter must be the exact
        // inverse of the comparator threshold, or a device set to position N
        // would latch a different POTxDAT count.
        for position in 0..=u8::MAX {
            let ohms = pot_position_resistance_ohms(position);
            assert!(ohms <= POT_MAX_RESISTANCE_OHMS);
            assert_eq!(
                Paula::pot_threshold_count(ohms),
                position,
                "position {position} (ohms {ohms})"
            );
        }
    }

    #[test]
    fn grounded_button_overrides_output_high_pot_charge() {
        let (mut paula, _) = paula_with_collect_sink();
        let pins = PotPins {
            left_x_released: false,
            ..PotPins::default()
        };
        paula.write_potgo(0x0301); // POT0X output-high + START
        for _ in 0..12 {
            paula.tick_pot_hsync(pins, 8);
        }
        assert_eq!(paula.read_potdat(0) & 0x00FF, 4);
        assert!(paula.pot_running());
    }

    #[test]
    fn intreq_write_reports_only_new_assertions() {
        let (mut paula, _) = paula_with_collect_sink();

        assert!(paula.write_intreq(0x8004));
        assert_eq!(paula.intreq, 0x0004);
        assert!(!paula.write_intreq(0x8004));
        assert_eq!(paula.intreq, 0x0004);

        assert!(!paula.write_intreq(0x0004));
        assert_eq!(paula.intreq, 0x0000);
        assert!(paula.write_intreq(0x8004));
        assert_eq!(paula.intreq, 0x0004);
    }

    #[test]
    fn intreq_latches_undocumented_int14_source() {
        let (mut paula, _) = paula_with_collect_sink();

        assert!(paula.write_intreq(0x8000 | INT_INT14));
        assert_eq!(paula.intreq & INT_INT14, INT_INT14);

        assert!(!paula.write_intreq(INT_INT14));
        assert_eq!(paula.intreq & INT_INT14, 0);
    }

    #[test]
    fn intreq_source_latch_wins_over_same_tick_clear() {
        let (mut paula, _) = paula_with_collect_sink();

        paula.serial_rx_buffer = Some(0x0355);
        assert!(paula.write_intreq_with_source_bits(INT_RBF, INT_RBF));
        assert_eq!(paula.intreq & INT_RBF, INT_RBF);
        assert_eq!(paula.serial_rx_buffer, Some(0x0355));
    }

    #[test]
    fn serdatr_rbf_bit_mirrors_intreq_latch() {
        let (mut paula, _) = paula_with_collect_sink();

        assert_eq!(paula.read_serdatr() & (1 << 14), 0);
        assert!(paula.write_intreq(0x8000 | INT_RBF));
        assert_ne!(paula.read_serdatr() & (1 << 14), 0);

        assert!(!paula.write_intreq(INT_RBF));
        assert_eq!(paula.read_serdatr() & (1 << 14), 0);
    }

    #[test]
    fn audpen_bits_mirror_audio_intreq_latches() {
        let (mut paula, _) = paula_with_collect_sink();

        assert!(paula.write_intreq(0x8000 | INT_AUD0 | INT_AUD3));
        assert_eq!(paula.audpen_bits(), 0b1001);

        assert!(!paula.write_intreq(INT_AUD0));
        assert_eq!(paula.audpen_bits(), 0b1000);
    }

    #[test]
    fn pending_ipl_maps_int14_to_level_six_priority() {
        assert_eq!(pending_ipl(INT_INT14), 6);
        assert_eq!(pending_ipl(INT_INT14 | INT_RBF), 6);
        assert_eq!(pending_ipl(INT_RBF), 5);
        assert_eq!(pending_ipl(INT_AUD0), 4);
        assert_eq!(pending_ipl(INT_TBE), 1);
    }
}
