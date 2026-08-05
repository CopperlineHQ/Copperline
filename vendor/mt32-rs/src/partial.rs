// SPDX-License-Identifier: LGPL-2.1-or-later

//! A partial: one voice of the chip, wired to its envelopes.
//!
//! Each partial owns an LA32 pair half, two ramps and the three envelopes,
//! and renders by asking them in the firmware's order: the amp ramp (whose
//! interrupt advances the amp envelope), the pitch (whose timer may re-aim
//! a sustaining amp), the cutoff ramp (whose interrupt advances the filter
//! envelope), then the chip. Ring-modulated structures put both halves of
//! the pair through the master's LA32; the slave partial keeps its own
//! envelopes and the master drives them.
//!
//! The patch cache is copied in at start: the reference keeps a pointer,
//! but every site that mutates a part's cache backs playing partials up
//! first, so the copy is what a partial would ever see. Live parameters
//! still come from memory through the cache's [`ParamSource`], which is
//! how SysEx reaches a sounding note.

use crate::jitter::Jitter;
use crate::la32::ramp::Ramp;
use crate::la32::wave::{IntPartialPair, Pair};
use crate::layout::Quirks;
use crate::note::{ParamSource, PatchCache, PolyStateKind};
use crate::param::PartialParam;
use crate::tables::Tables;
use crate::tva::{Tva, TvaHost};
use crate::tvf::{Tvf, TvfHost};
use crate::tvp::{Tvp, TvpHost};

/// The pan numerators for a split pair (structure mix 3): how setting maps
/// to each side's share.
const PAN_NUMERATOR_MASTER: [u8; 15] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7];
const PAN_NUMERATOR_SLAVE: [u8; 15] = [0, 1, 2, 3, 4, 5, 6, 7, 7, 7, 7, 7, 7, 7, 7];

/// The integer pan factors: `0.5 + i * 8192 / 14`, with the zeroth entry
/// zero as the reference's uninitialised static is.
fn pan_factor(setting: i32) -> i32 {
    if setting == 0 {
        0
    } else {
        (0.5 + setting as f64 * 8192.0 / 14.0) as i32
    }
}

/// The host options the reference exposes; the defaults are its defaults.
#[derive(Debug, Clone, Copy)]
pub struct NiceOptions {
    pub amp_ramp: bool,
    pub panning: bool,
    pub partial_mixing: bool,
    pub reversed_stereo: bool,
}

impl Default for NiceOptions {
    fn default() -> NiceOptions {
        NiceOptions {
            amp_ramp: true,
            panning: false,
            partial_mixing: false,
            reversed_stereo: false,
        }
    }
}

/// Everything a partial needs from the part and note to start.
#[derive(Debug, Clone, Copy)]
pub struct StartArgs<'a> {
    pub cache: &'a PatchCache,
    /// A rhythm note's panpot, or the part's.
    pub pan_setting: u8,
    pub key: u32,
    pub velocity: u32,
    pub sustain: bool,
    /// Which slot this partial occupies in the engine's table; the odd
    /// half of each four flips phase unless nice mixing is on.
    pub partial_index: usize,
    /// Whether the layout's PCM directory runs past 128 waves, which is
    /// where waveform bits select the second bank.
    pub extended_pcm: bool,
    pub options: NiceOptions,
    /// The rhythm key a drum note plays from, for its live setup reads.
    pub rhythm_key: Option<usize>,
}

/// The live values the envelopes read each sample, assembled by the engine
/// from the part and system state.
#[derive(Debug, Clone, Copy)]
pub struct LiveValues {
    pub part_volume: u8,
    pub expression: u8,
    pub rhythm_output_level: Option<u8>,
    pub master_vol: u8,
    pub master_tune_pitch_delta: i32,
    pub pitch_bend: i32,
    pub modulation: u32,
    pub patch_key_shift: u8,
    pub patch_fine_tune: u8,
    pub nice_amp_ramp: bool,
}

/// One of the chip's thirty-two voices.
#[derive(Debug, Clone)]
pub struct Partial {
    owner_part: Option<usize>,
    poly: Option<usize>,
    pair: Option<usize>,
    pub(crate) la32: IntPartialPair,
    amp_ramp: Ramp,
    cutoff_ramp: Ramp,
    tva: Tva,
    tvf: Tvf,
    tvp: Tvp,
    cache: Option<PatchCache>,
    mix_type: u32,
    structure_position: i32,
    left_pan: i32,
    right_pan: i32,
    /// The wave directory entry a PCM partial plays, resolved at start.
    pcm_wave: Option<usize>,
    pulse_width_val: u32,
    key: u32,
    velocity: u32,
    sustain: bool,
    rhythm_key: Option<usize>,
    pub(crate) already_outputed: bool,
}

impl Partial {
    pub fn new(quirks: &Quirks) -> Partial {
        Partial {
            owner_part: None,
            poly: None,
            pair: None,
            la32: IntPartialPair::new(),
            amp_ramp: Ramp::new(),
            cutoff_ramp: Ramp::new(),
            tva: Tva::new(),
            tvf: Tvf::new(),
            tvp: Tvp::new(quirks),
            cache: None,
            mix_type: 0,
            structure_position: 0,
            left_pan: 0,
            right_pan: 0,
            pcm_wave: None,
            pulse_width_val: 0,
            key: 0,
            velocity: 0,
            sustain: false,
            rhythm_key: None,
            already_outputed: false,
        }
    }

    pub fn is_active(&self) -> bool {
        self.owner_part.is_some()
    }

    pub fn owner_part(&self) -> Option<usize> {
        self.owner_part
    }

    pub fn poly(&self) -> Option<usize> {
        self.poly
    }

    pub fn pair(&self) -> Option<usize> {
        self.pair
    }

    pub fn set_pair(&mut self, pair: Option<usize>) {
        self.pair = pair;
    }

    /// Claim the slot for a part; the engine's allocator calls this.
    pub fn activate(&mut self, part: usize) {
        self.owner_part = Some(part);
    }

    /// Release the slot's own state. The engine handles everything that
    /// cascades: the free list, the poly, the pair's LA32 halves.
    pub fn clear_for_deactivation(&mut self) {
        self.owner_part = None;
        self.poly = None;
    }

    pub fn has_ring_modulating_slave(&self) -> bool {
        self.pair.is_some() && self.structure_position == 0 && matches!(self.mix_type, 1 | 2)
    }

    pub fn is_ring_modulating_slave(&self) -> bool {
        self.pair.is_some() && self.structure_position == 1 && matches!(self.mix_type, 1 | 2)
    }

    pub fn is_ring_modulating_no_mix(&self) -> bool {
        self.pair.is_some()
            && ((self.structure_position == 1 && self.mix_type == 1) || self.mix_type == 2)
    }

    pub fn is_pcm(&self) -> bool {
        self.pcm_wave.is_some()
    }

    pub fn pcm_wave(&self) -> Option<usize> {
        self.pcm_wave
    }

    pub fn rhythm_key(&self) -> Option<usize> {
        self.rhythm_key
    }

    pub fn mix_type(&self) -> u32 {
        self.mix_type
    }

    /// Whether this partial's output goes to the reverb mix.
    pub fn should_reverb(&self) -> bool {
        self.is_active() && self.cache.as_ref().is_some_and(|c| c.reverb)
    }

    pub fn key(&self) -> u32 {
        self.key
    }

    pub fn velocity(&self) -> u32 {
        self.velocity
    }

    pub fn can_sustain(&self) -> bool {
        self.sustain
    }

    pub fn param_source(&self) -> Option<ParamSource> {
        self.cache.as_ref().map(|c| c.param_source)
    }

    /// The snapshot parameters the note started with.
    pub fn src_partial(&self) -> Option<PartialParam<'_>> {
        self.cache.as_ref().map(|c| PartialParam(&c.src_partial))
    }

    /// Start this partial for a note. The LA32 setup is returned for the
    /// engine to apply to whichever pair owns the sound -- the master's
    /// own, or the master's on behalf of this slave.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &mut self,
        tables: &Tables,
        quirks: &Quirks,
        args: StartArgs,
        poly: usize,
        pair: Option<usize>,
        waves: &[crate::pcm::Wave],
        live: &LiveValues,
    ) -> La32Setup {
        let cache = *args.cache;
        self.poly = Some(poly);
        self.key = args.key;
        self.velocity = args.velocity;
        self.sustain = args.sustain;
        self.rhythm_key = args.rhythm_key;
        self.mix_type = cache.structure_mix;
        self.structure_position = cache.structure_position;

        let mut pan_setting = args.pan_setting;
        let mut pair = pair;
        let mut mix_type = self.mix_type;
        if mix_type == 3 {
            // A split pair goes to opposite sides and stops being a pair.
            pan_setting = if self.structure_position == 0 {
                PAN_NUMERATOR_MASTER[usize::from(pan_setting)] << 1
            } else {
                PAN_NUMERATOR_SLAVE[usize::from(pan_setting)] << 1
            };
            mix_type = 0;
            pair = None;
        } else if !args.options.panning {
            // The hardware's pan resolution is seven positions a side.
            pan_setting &= 0x0E;
        }
        self.mix_type = mix_type;
        self.pair = pair;
        let left_setting = if args.options.reversed_stereo {
            14 - i32::from(pan_setting)
        } else {
            i32::from(pan_setting)
        };
        self.left_pan = pan_factor(left_setting);
        self.right_pan = pan_factor(14 - left_setting);
        if !args.options.partial_mixing && args.partial_index & 4 != 0 {
            // Half the voices run phase-reversed, as on the board.
            self.left_pan = -self.left_pan;
            self.right_pan = -self.right_pan;
        }

        if cache.pcm_partial {
            let mut pcm_num = usize::from(cache.pcm);
            if args.extended_pcm && cache.waveform > 1 {
                pcm_num += 128;
            }
            self.pcm_wave = Some(pcm_num);
        } else {
            self.pcm_wave = None;
        }

        let src = PartialParam(&cache.src_partial);
        let mut pulse_width = (args.velocity as i32 - 64)
            * (i32::from(src.pulse_width_velo_sensitivity()) - 7)
            + i32::from(tables.pulse_width_100_to_255[usize::from(src.pulse_width())]);
        pulse_width = pulse_width.clamp(0, 255);
        self.pulse_width_val = pulse_width as u32;

        self.already_outputed = false;
        self.cache = Some(cache);

        let tva_host = tva_host_for(self, live);
        let tvp_host = tvp_host_for(self, waves, live);
        let tvf_host = tvf_host_for(self);
        // At start the live parameters are exactly what the cache
        // snapshotted a moment ago.
        let live_param = PartialParam(&cache.src_partial);
        self.tva
            .reset(tables, &mut self.amp_ramp, live_param, &tva_host, quirks);
        self.tvp.reset(live_param, &tvp_host, quirks);
        self.tvf.reset(
            tables,
            &mut self.cutoff_ramp,
            live_param,
            &tvf_host,
            quirks,
            self.tvp.base_pitch(),
        );

        let resonance = live_param.tvf_resonance() + 1;
        La32Setup {
            slave_side: self.is_ring_modulating_slave(),
            init_pair: (!self.is_ring_modulating_slave())
                .then_some((self.has_ring_modulating_slave(), self.mix_type == 1)),
            pcm: self.pcm_wave.map(|n| {
                let wave = waves[n];
                (wave.addr, wave.len, wave.looped)
            }),
            synth: (!cache.pcm_partial).then_some((
                cache.waveform & 1 != 0,
                self.pulse_width_val as u8,
                resonance,
            )),
        }
    }

    /// The amp for this sample: the ramp inverted from full scale, with
    /// its interrupt advancing the amp envelope.
    pub fn next_amp(
        &mut self,
        tables: &Tables,
        param: PartialParam,
        live: &LiveValues,
        quirks: &Quirks,
    ) -> u32 {
        let amp = 67117056 - self.amp_ramp.next_value();
        if self.amp_ramp.check_interrupt() {
            let host = tva_host_for(self, live);
            self.tva
                .handle_interrupt(tables, &mut self.amp_ramp, param, &host, quirks);
        }
        amp
    }

    /// The pitch for this sample, with the timer's sustain re-aim
    /// forwarded to the amp envelope as the firmware does.
    pub fn next_pitch(
        &mut self,
        tables: &Tables,
        param: PartialParam,
        waves: &[crate::pcm::Wave],
        live: &LiveValues,
        quirks: &Quirks,
        jitter: &mut Jitter,
    ) -> u16 {
        let host = tvp_host_for(self, waves, live);
        let pitch = self.tvp.next_pitch(param, &host, quirks, jitter);
        if self.tvp.take_pitch_updated() {
            let tva_host = tva_host_for(self, live);
            self.tva
                .recalc_sustain(tables, &mut self.amp_ramp, param, &tva_host, quirks);
        }
        pitch
    }

    /// The cutoff for this sample: zero for PCM, else the base plus the
    /// ramp, whose interrupt advances the filter envelope.
    pub fn next_cutoff(&mut self, tables: &Tables, param: PartialParam) -> u32 {
        if self.is_pcm() {
            return 0;
        }
        let value = self.cutoff_ramp.next_value();
        if self.cutoff_ramp.check_interrupt() {
            let host = tvf_host_for(self);
            self.tvf
                .handle_interrupt(tables, &mut self.cutoff_ramp, param, &host);
        }
        (u32::from(self.tvf.base_cutoff()) << 18) + value
    }

    /// Whether the voice still sounds: the amp envelope alive and the LA32
    /// half active.
    pub fn still_playing(&self, half: Pair) -> bool {
        self.tva.is_playing() && self.la32.is_active(half)
    }

    pub fn tva_playing(&self) -> bool {
        self.tva.is_playing()
    }

    /// Mix one pair output sample into the buffers through the pans, with
    /// the reference's saturation.
    pub fn mix_sample(&self, sample: i16, left: &mut i16, right: &mut i16) {
        let left_out = ((i32::from(sample) * self.left_pan) >> 13) + i32::from(*left);
        let right_out = ((i32::from(sample) * self.right_pan) >> 13) + i32::from(*right);
        *left = clip(left_out);
        *right = clip(right_out);
    }

    /// The note lets go: all three envelopes fall.
    pub fn start_decay_all(&mut self, tables: &Tables, param: PartialParam) {
        self.tva.start_decay(tables, &mut self.amp_ramp, param);
        self.tvp.start_decay();
        self.tvf.start_decay(tables, &mut self.cutoff_ramp, param);
    }

    /// The allocator steals the voice: the amp falls fast, nothing else.
    pub fn start_abort(&mut self, tables: &Tables) {
        self.tva.start_abort(tables, &mut self.amp_ramp);
    }
}

impl Partial {
    /// Apply a start's LA32 setup to this partial's own pair: the
    /// unpaired and master cases, where the sound lives here. A slave's
    /// setup goes to its master's pair instead, which the engine owns.
    pub fn apply_own_la32_setup(&mut self, tables: &Tables, setup: La32Setup) {
        debug_assert!(!setup.slave_side, "a slave's setup goes to the master");
        if let Some((ring, mixed)) = setup.init_pair {
            self.la32.init(ring, mixed);
        }
        if let Some((addr, len, looped)) = setup.pcm {
            self.la32.init_pcm(Pair::Master, addr, len, looped);
        } else if let Some((sawtooth, pulse_width, resonance)) = setup.synth {
            self.la32
                .init_synth(tables, Pair::Master, sawtooth, pulse_width, resonance);
        }
        if !self.has_ring_modulating_slave() {
            self.la32.deactivate(Pair::Slave);
        }
    }

    /// Render one sample of an unpaired partial into the buffers: the
    /// firmware's order -- amp, pitch, cutoff -- then the chip, then the
    /// pans. False when the voice has died; the engine deactivates.
    #[allow(clippy::too_many_arguments)]
    pub fn render_solo_sample(
        &mut self,
        tables: &Tables,
        pcm: &[i16],
        waves: &[crate::pcm::Wave],
        param: PartialParam,
        live: &LiveValues,
        quirks: &Quirks,
        jitter: &mut Jitter,
        left: &mut i16,
        right: &mut i16,
    ) -> bool {
        if !self.still_playing(Pair::Master) {
            return false;
        }
        let amp = self.next_amp(tables, param, live, quirks);
        let pitch = self.next_pitch(tables, param, waves, live, quirks, jitter);
        let cutoff = self.next_cutoff(tables, param);
        self.la32
            .generate_next_sample(tables, pcm, Pair::Master, amp, pitch, cutoff);
        let sample = self.la32.next_out_sample(tables);
        self.mix_sample(sample, left, right);
        true
    }
}

/// What the engine applies to an LA32 pair after a start: the master's
/// own, or the master's on a slave's behalf.
#[derive(Debug, Clone, Copy)]
pub struct La32Setup {
    pub slave_side: bool,
    /// Ring modulation and mixing for the structure, master side only.
    pub init_pair: Option<(bool, bool)>,
    /// A PCM voice: address, length, looped.
    pub pcm: Option<(u32, u32, bool)>,
    /// A synth voice: sawtooth, pulse width, resonance.
    pub synth: Option<(bool, u8, u8)>,
}

/// Assemble the amp envelope's view of the world.
pub fn tva_host_for(partial: &Partial, live: &LiveValues) -> TvaHost {
    TvaHost {
        key: partial.key,
        velocity: partial.velocity,
        part_volume: live.part_volume,
        expression: live.expression,
        rhythm_output_level: live.rhythm_output_level,
        master_vol: live.master_vol,
        can_sustain: partial.sustain,
        ring_modulating_slave: partial.is_ring_modulating_slave(),
        ring_modulating_no_mix: partial.is_ring_modulating_no_mix(),
        nice_amp_ramp: live.nice_amp_ramp,
    }
}

/// And the pitch envelope's.
pub fn tvp_host_for(partial: &Partial, waves: &[crate::pcm::Wave], live: &LiveValues) -> TvpHost {
    TvpHost {
        key: partial.key,
        velocity: partial.velocity,
        pcm: partial.pcm_wave.map(|n| {
            let wave = waves[n];
            (wave.pitch, wave.master_tune_immune)
        }),
        master_tune_pitch_delta: live.master_tune_pitch_delta,
        pitch_bend: live.pitch_bend,
        modulation: live.modulation,
        patch_key_shift: live.patch_key_shift,
        patch_fine_tune: live.patch_fine_tune,
    }
}

/// And the filter envelope's.
pub fn tvf_host_for(partial: &Partial) -> TvfHost {
    TvfHost {
        key: partial.key,
        velocity: partial.velocity,
        can_sustain: partial.sustain,
    }
}

/// The reference's sample clip: in range passes, out of range saturates
/// by the sign bit's arithmetic, exactly as it writes it.
fn clip(sample: i32) -> i16 {
    if (-0x8000..=0x7FFF).contains(&sample) {
        sample as i16
    } else {
        ((sample >> 31) ^ 0x7FFF) as i16
    }
}

/// The poly state a partial's phase implies, for the engine's reporting.
pub fn poly_state_for_phase(phase: i32) -> PolyStateKind {
    if phase >= crate::tva::TVA_PHASE_RELEASE {
        PolyStateKind::Releasing
    } else {
        PolyStateKind::Playing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LAYOUTS;
    use crate::note::{cache_timbre, ParamSource, PatchCache};
    use crate::param::TimbreCommon;

    /// The first whole note through this crate's own chain: a square
    /// timbre started, sustained, released, and dead -- audio out the
    /// far end, silence after the release runs dry.
    #[test]
    fn a_note_sounds_and_dies_through_the_whole_chain() {
        let tables = Tables::new();
        let quirks = &LAYOUTS[8].quirks;
        let jitter = &mut Jitter::new();
        let pcm = vec![0i16; 0x1000];
        let waves = Vec::new();

        // A playable square timbre: one partial, centred tuning, a quick
        // envelope with a real sustain.
        let mut common = [0u8; 14];
        common[10] = 0; // structure 1: synth + synth, mixing
        common[12] = 0b0001; // only partial 1 plays
        let mut partial_bytes = [0u8; 58];
        partial_bytes[0] = 36; // pitch coarse centred
        partial_bytes[1] = 50; // fine centred
        partial_bytes[2] = 11; // keyfollow 1
        partial_bytes[6] = 50; // pulse width
        partial_bytes[8] = 0; // no pitch envelope depth
        for n in 0..5 {
            partial_bytes[15 + n] = 50; // pitch env flat
        }
        partial_bytes[23] = 80; // cutoff
        partial_bytes[24] = 8; // resonance
        partial_bytes[28] = 50; // tvf env depth
        for n in 0..5 {
            partial_bytes[32 + n] = 10; // tvf times
            partial_bytes[37 + n.min(3)] = 60; // tvf levels
        }
        partial_bytes[41] = 100; // tva level
        partial_bytes[42] = 50; // velocity sensitivity neutral
        for n in 0..5 {
            partial_bytes[49 + n] = 10; // tva times
        }
        partial_bytes[54] = 100;
        partial_bytes[55] = 90;
        partial_bytes[56] = 90;
        partial_bytes[57] = 80; // sustain level

        let zeros = [0u8; 58];
        let params = [&partial_bytes[..], &zeros[..], &zeros[..], &zeros[..]];
        let mut cache = [PatchCache::empty(ParamSource::TimbreTemp {
            part: 0,
            partial: 0,
        }); 4];
        cache_timbre(&mut cache, TimbreCommon(&common), params, |t| {
            ParamSource::TimbreTemp {
                part: 0,
                partial: t,
            }
        });
        assert!(cache[0].play_partial && !cache[0].pcm_partial);

        let live = LiveValues {
            part_volume: 100,
            expression: 100,
            rhythm_output_level: None,
            master_vol: 100,
            master_tune_pitch_delta: 0,
            pitch_bend: 0,
            modulation: 0,
            patch_key_shift: 24,
            patch_fine_tune: 50,
            nice_amp_ramp: true,
        };
        let mut partial = Partial::new(quirks);
        partial.activate(0);
        let setup = partial.start(
            &tables,
            quirks,
            StartArgs {
                cache: &cache[0],
                pan_setting: 7,
                key: 60,
                velocity: 100,
                sustain: true,
                partial_index: 0,
                extended_pcm: false,
                options: NiceOptions::default(),
                rhythm_key: None,
            },
            0,
            None,
            &waves,
            &live,
        );
        partial.apply_own_la32_setup(&tables, setup);

        let param = PartialParam(&partial_bytes);
        let mut peak: i16 = 0;
        for _ in 0..32_000 {
            let (mut l, mut r) = (0i16, 0i16);
            assert!(partial.render_solo_sample(
                &tables, &pcm, &waves, param, &live, quirks, jitter, &mut l, &mut r
            ));
            peak = peak
                .max(l.unsigned_abs() as i16)
                .max(r.unsigned_abs() as i16);
        }
        assert!(peak > 1000, "a held note makes real sound: peak {peak}");

        partial.start_decay_all(&tables, param);
        let mut died_at = None;
        for n in 0..640_000 {
            let (mut l, mut r) = (0i16, 0i16);
            if !partial.render_solo_sample(
                &tables, &pcm, &waves, param, &live, quirks, jitter, &mut l, &mut r,
            ) {
                died_at = Some(n);
                break;
            }
        }
        let died_at = died_at.expect("the release runs out");
        assert!(died_at > 100, "the release takes real time: {died_at}");
        assert!(!partial.tva_playing());
    }
}
