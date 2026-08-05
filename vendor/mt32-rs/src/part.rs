// SPDX-License-Identifier: LGPL-2.1-or-later

//! A part: one of the module's eight melodic channels, or the rhythm
//! channel that is the ninth.
//!
//! The part owns what the firmware keeps per channel: the controllers
//! (bend, modulation, expression, the hold pedal, the RPN machinery), the
//! patch cache its notes start from, the ordered list of sounding polys,
//! and the counters that tell the display when the channel falls silent.
//! Everything that reaches across objects -- starting notes, stealing
//! partials, walking a decay through a poly's partials -- lives in the
//! engine; the part answers questions and keeps its own books.
//!
//! The rhythm part differs as the firmware has it differ: a cache per
//! mapped drum instead of one per channel, notes addressed by key, pan and
//! level from the rhythm setup, and no timbre temp at all.

use crate::layout::Quirks;
use crate::memory::Memory;
use crate::note::{cache_timbre, ParamSource, PatchCache, PolyStateKind, StateChange};
use crate::param::{PatchTemp, RhythmTemp, TimbreCommon};

/// The rhythm part's number, and how many parts there are.
pub const RHYTHM_PART: usize = 8;
pub const PART_COUNT: usize = 9;

/// How many rhythm keys the map covers.
pub const RHYTHM_KEYS: usize = 85;

/// One part's books.
#[derive(Debug, Clone)]
pub struct Part {
    part_num: usize,
    /// The cache melodic notes start from.
    patch_cache: [PatchCache; 4],
    /// The rhythm part's per-drum caches; empty on melodic parts.
    drum_cache: Vec<[PatchCache; 4]>,
    /// Sounding polys, oldest first unless single assign prepends.
    active_polys: Vec<usize>,
    expression: u8,
    modulation: u8,
    pitch_bend: i32,
    pitch_bender_range: i32,
    hold_pedal: bool,
    /// 255 means none; zero silences the part outright.
    volume_override: u8,
    rpn: u16,
    nrpn: bool,
    /// The name of the timbre the part is playing, for the display.
    current_instr: [u8; 10],
    active_partial_count: u32,
    active_non_releasing_poly_count: u32,
}

impl Part {
    pub fn new(part_num: usize) -> Part {
        let source = |t| ParamSource::TimbreTemp {
            part: part_num,
            partial: t,
        };
        Part {
            part_num,
            patch_cache: [
                PatchCache::empty(source(0)),
                PatchCache::empty(source(1)),
                PatchCache::empty(source(2)),
                PatchCache::empty(source(3)),
            ],
            drum_cache: if part_num == RHYTHM_PART {
                let empty = [
                    PatchCache::empty(ParamSource::Bank {
                        timbre: 0,
                        partial: 0,
                    }),
                    PatchCache::empty(ParamSource::Bank {
                        timbre: 0,
                        partial: 1,
                    }),
                    PatchCache::empty(ParamSource::Bank {
                        timbre: 0,
                        partial: 2,
                    }),
                    PatchCache::empty(ParamSource::Bank {
                        timbre: 0,
                        partial: 3,
                    }),
                ];
                vec![empty; RHYTHM_KEYS]
            } else {
                Vec::new()
            },
            active_polys: Vec::new(),
            expression: 100,
            modulation: 0,
            pitch_bend: 0,
            pitch_bender_range: 0,
            hold_pedal: false,
            volume_override: 255,
            rpn: 0xFFFF,
            nrpn: false,
            current_instr: [0; 10],
            active_partial_count: 0,
            active_non_releasing_poly_count: 0,
        }
    }

    pub fn part_num(&self) -> usize {
        self.part_num
    }

    pub fn is_rhythm(&self) -> bool {
        self.part_num == RHYTHM_PART
    }

    // ------------------------------------------------------------------
    // Controllers, with the firmware's own conversions.

    /// CC7: the control ROM's own scaling onto 0-100.
    pub fn set_volume(&self, memory: &mut Memory, midi_volume: u32) {
        memory.patch_temp_mut(self.part_num)[8] = (midi_volume * 100 / 127) as u8;
    }

    /// The part's playing volume: the override when one is set, else the
    /// patch temp level.
    pub fn volume(&self, memory: &Memory) -> u8 {
        if self.volume_override <= 100 {
            self.volume_override
        } else {
            PatchTemp(memory.patch_temp(self.part_num)).output_level()
        }
    }

    /// A host-side override; zero silences the part entirely, which the
    /// engine enforces by dropping its note-ons.
    pub fn set_volume_override(&mut self, volume: u8) {
        self.volume_override = volume;
    }

    pub fn volume_override(&self) -> u8 {
        self.volume_override
    }

    /// CC11, the same scaling as the volume.
    pub fn set_expression(&mut self, midi_expression: u32) {
        self.expression = (midi_expression * 100 / 127) as u8;
    }

    pub fn expression(&self) -> u8 {
        self.expression
    }

    pub fn set_modulation(&mut self, midi_modulation: u32) {
        self.modulation = midi_modulation as u8;
    }

    pub fn modulation(&self) -> u8 {
        self.modulation
    }

    /// CC10: the elder units divide by nine; the later ones spread the
    /// range with a shift and a stranger divisor.
    pub fn set_pan(&self, memory: &mut Memory, midi_pan: u32, quirks: &Quirks) {
        let panpot = if quirks.pan_mult {
            midi_pan / 9
        } else {
            (midi_pan << 3) / 68
        };
        memory.patch_temp_mut(self.part_num)[9] = panpot as u8;
    }

    /// The bender range moved: recompute the multiplier the bend uses.
    pub fn update_pitch_bender_range(&mut self, memory: &Memory) {
        self.pitch_bender_range =
            i32::from(PatchTemp(memory.patch_temp(self.part_num)).bender_range()) * 683;
    }

    /// The wheel: centred at 8192, scaled by the range.
    pub fn set_bend(&mut self, midi_bend: u32) {
        self.pitch_bend = ((midi_bend as i32 - 8192) * self.pitch_bender_range) >> 14;
    }

    pub fn pitch_bend(&self) -> i32 {
        self.pitch_bend
    }

    /// CC64. True on the release edge -- the moment held notes decay --
    /// and the engine then walks the pedal hold off every poly.
    pub fn set_hold_pedal(&mut self, pressed: bool) -> bool {
        if self.hold_pedal && !pressed {
            self.hold_pedal = false;
            true
        } else {
            self.hold_pedal = pressed;
            false
        }
    }

    pub fn hold_pedal(&self) -> bool {
        self.hold_pedal
    }

    /// CC6, honoured only for RPN zero: the bender range, capped at two
    /// octaves.
    pub fn set_data_entry_msb(&mut self, memory: &mut Memory, value: u8) -> bool {
        if self.nrpn || self.rpn != 0 {
            return false;
        }
        memory.patch_temp_mut(self.part_num)[4] = value.min(24);
        self.update_pitch_bender_range(memory);
        true
    }

    pub fn set_nrpn(&mut self) {
        self.nrpn = true;
    }

    pub fn set_rpn_lsb(&mut self, value: u8) {
        self.nrpn = false;
        self.rpn = (self.rpn & 0xFF00) | u16::from(value);
    }

    pub fn set_rpn_msb(&mut self, value: u8) {
        self.nrpn = false;
        self.rpn = (self.rpn & 0x00FF) | (u16::from(value) << 8);
    }

    /// Reset All Controllers, as the firmware resets them. Returns whether
    /// a pedal release fell out of it.
    pub fn reset_all_controllers(&mut self) -> bool {
        self.modulation = 0;
        self.expression = 100;
        self.pitch_bend = 0;
        self.set_hold_pedal(false)
    }

    // ------------------------------------------------------------------
    // Keys and caches.

    /// A MIDI key onto the internal key: the elder units leave it alone
    /// (their key shift lives in the pitch envelope); the later apply the
    /// patch's shift and wrap the result an octave at a time into range.
    pub fn midi_key_to_key(&self, memory: &Memory, quirks: &Quirks, midi_key: u32) -> u32 {
        if quirks.key_shift {
            return midi_key;
        }
        let mut key =
            midi_key as i32 + i32::from(PatchTemp(memory.patch_temp(self.part_num)).key_shift());
        while key < 36 {
            key += 12;
        }
        while key > 132 {
            key -= 12;
        }
        (key - 24) as u32
    }

    /// The melodic cache, recomputed from the part's timbre temp. The
    /// engine backs playing partials up before calling.
    pub fn cache_timbre_from_temp(&mut self, memory: &Memory) {
        let part = self.part_num;
        let timbre = memory.timbre_temp(part);
        let (common, partials) = timbre.split_at(crate::memory::TIMBRE_COMMON);
        let p: Vec<&[u8]> = partials
            .chunks_exact(crate::memory::TIMBRE_PARTIAL)
            .collect();
        cache_timbre(
            &mut self.patch_cache,
            TimbreCommon(common),
            [p[0], p[1], p[2], p[3]],
            |t| ParamSource::TimbreTemp { part, partial: t },
        );
        self.current_instr.copy_from_slice(&common[..10]);
    }

    /// A drum's cache, recomputed from its bank timbre.
    pub fn cache_drum_timbre(&mut self, memory: &Memory, drum: usize, abs_timbre: usize) {
        let timbre = memory.bank_timbre(abs_timbre);
        let (common, partials) = timbre.split_at(crate::memory::TIMBRE_COMMON);
        let p: Vec<&[u8]> = partials
            .chunks_exact(crate::memory::TIMBRE_PARTIAL)
            .collect();
        cache_timbre(
            &mut self.drum_cache[drum],
            TimbreCommon(common),
            [p[0], p[1], p[2], p[3]],
            |t| ParamSource::Bank {
                timbre: abs_timbre,
                partial: t,
            },
        );
        self.current_instr.copy_from_slice(&common[..10]);
    }

    /// The part's own refresh bookkeeping: caches dirtied and re-switched,
    /// the instrument name re-read, the bender range recomputed. The
    /// engine backs partials up first and tells the display after.
    pub fn refresh(&mut self, memory: &Memory) {
        let reverb = PatchTemp(memory.patch_temp(self.part_num)).reverb_switch() > 0;
        for slot in self.patch_cache.iter_mut() {
            slot.dirty = true;
            slot.reverb = reverb;
        }
        if !self.is_rhythm() {
            let name: [u8; 10] = memory.timbre_temp(self.part_num)[..10].try_into().unwrap();
            self.current_instr = name;
        }
        self.update_pitch_bender_range(memory);
    }

    /// The rhythm part's refresh: every mapped drum dirtied with its own
    /// reverb switch.
    pub fn refresh_rhythm(&mut self, memory: &Memory, rhythm_settings_count: usize) {
        for drum in 0..rhythm_settings_count.min(RHYTHM_KEYS) {
            let setup = RhythmTemp(memory.rhythm_temp(drum));
            if setup.timbre() >= 127 {
                continue;
            }
            let reverb = setup.reverb_switch() > 0;
            for slot in self.drum_cache[drum].iter_mut() {
                slot.dirty = true;
                slot.reverb = reverb;
            }
        }
        self.update_pitch_bender_range(memory);
    }

    /// A timbre in the banks changed: the caches reading it dirty.
    pub fn refresh_timbre(&mut self, memory: &Memory, abs_timbre: usize) {
        if self.is_rhythm() {
            for drum in 0..self.drum_cache.len() {
                if usize::from(RhythmTemp(memory.rhythm_temp(drum)).timbre()) == abs_timbre - 128 {
                    self.drum_cache[drum][0].dirty = true;
                }
            }
        } else if self.abs_timbre_num(memory) == abs_timbre {
            let name: [u8; 10] = memory.timbre_temp(self.part_num)[..10].try_into().unwrap();
            self.current_instr = name;
            self.patch_cache[0].dirty = true;
        }
    }

    /// Which bank timbre the part's patch names.
    pub fn abs_timbre_num(&self, memory: &Memory) -> usize {
        let patch = PatchTemp(memory.patch_temp(self.part_num));
        usize::from(patch.timbre_group()) * 64 + usize::from(patch.timbre_num())
    }

    pub fn patch_cache(&self) -> &[PatchCache; 4] {
        &self.patch_cache
    }

    pub fn drum_cache(&self, drum: usize) -> &[PatchCache; 4] {
        &self.drum_cache[drum]
    }

    pub fn current_instr(&self) -> &[u8; 10] {
        &self.current_instr
    }

    // ------------------------------------------------------------------
    // The poly list and the counters.

    /// Take a poly on: single-assign patches prepend so the newest is the
    /// first stolen; everything else queues.
    pub fn add_poly(&mut self, poly: usize, prepend: bool) {
        if prepend {
            self.active_polys.insert(0, poly);
        } else {
            self.active_polys.push(poly);
        }
    }

    pub fn remove_poly(&mut self, poly: usize) {
        self.active_polys.retain(|&p| p != poly);
    }

    pub fn active_polys(&self) -> &[usize] {
        &self.active_polys
    }

    pub fn active_partial_count(&self) -> u32 {
        self.active_partial_count
    }

    pub fn note_partials_started(&mut self, count: u32) {
        self.active_partial_count += count;
    }

    pub fn partial_deactivated(&mut self) {
        self.active_partial_count -= 1;
    }

    /// A poly's state moved: keep the non-releasing count, and say when
    /// the whole part starts or stops sounding, which is what the display
    /// marks. The rhythm part's marks blink elsewhere; it reports nothing.
    pub fn poly_state_changed(&mut self, change: StateChange) -> Option<bool> {
        if self.is_rhythm() {
            return None;
        }
        match change.new {
            PolyStateKind::Playing => {
                self.active_non_releasing_poly_count += 1;
                if self.active_non_releasing_poly_count == 1 {
                    return Some(true);
                }
            }
            PolyStateKind::Releasing | PolyStateKind::Inactive => {
                if matches!(change.old, PolyStateKind::Playing | PolyStateKind::Held) {
                    self.active_non_releasing_poly_count -= 1;
                    if self.active_non_releasing_poly_count == 0 {
                        return Some(false);
                    }
                }
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LAYOUTS;

    fn a_memory() -> Memory {
        let image = vec![0x7F; 64 * 1024];
        Memory::power_on(&image, &LAYOUTS[0]).expect("opens")
    }

    /// The controller conversions are the firmware's: volume and
    /// expression on the 100/127 scale, the bend through the range
    /// multiplier, both pan divisors.
    #[test]
    fn the_controllers_convert_as_the_firmware_does() {
        let mut memory = a_memory();
        let mut part = Part::new(0);
        part.set_volume(&mut memory, 127);
        assert_eq!(part.volume(&memory), 100);
        part.set_volume(&mut memory, 64);
        assert_eq!(part.volume(&memory), 50);
        part.set_volume_override(30);
        assert_eq!(part.volume(&memory), 30, "the override wins while set");
        part.set_volume_override(255);

        part.set_expression(127);
        assert_eq!(part.expression(), 100);

        memory.patch_temp_mut(0)[4] = 12; // bender range: one octave
        part.update_pitch_bender_range(&memory);
        part.set_bend(16383);
        assert_eq!(part.pitch_bend(), (8191 * 12 * 683) >> 14);
        part.set_bend(0);
        assert_eq!(part.pitch_bend(), (-8192 * 12 * 683) >> 14);

        let elder = &LAYOUTS[0].quirks;
        let later = &LAYOUTS[8].quirks;
        part.set_pan(&mut memory, 127, elder);
        assert_eq!(memory.patch_temp(0)[9], 14);
        part.set_pan(&mut memory, 127, later);
        assert_eq!(memory.patch_temp(0)[9], 14);
        part.set_pan(&mut memory, 64, later);
        assert_eq!(memory.patch_temp(0)[9], 7);
    }

    /// The later units shift keys by the patch and wrap octaves into
    /// range; the elder leave the key alone for the pitch envelope.
    #[test]
    fn the_key_shift_belongs_to_the_generation() {
        let mut memory = a_memory();
        let part = Part::new(0);
        let elder = &LAYOUTS[0].quirks;
        let later = &LAYOUTS[8].quirks;
        memory.patch_temp_mut(0)[2] = 24; // shift centred: no change
        assert_eq!(part.midi_key_to_key(&memory, later, 60), 60);
        assert_eq!(part.midi_key_to_key(&memory, elder, 60), 60);
        memory.patch_temp_mut(0)[2] = 48; // +24 semitones
        assert_eq!(part.midi_key_to_key(&memory, later, 60), 84);
        assert_eq!(
            part.midi_key_to_key(&memory, elder, 60),
            60,
            "elder untouched"
        );
        // Off the top: octaves fold back until the key fits.
        assert_eq!(part.midi_key_to_key(&memory, later, 120), 108);
        // Off the bottom likewise.
        memory.patch_temp_mut(0)[2] = 0; // -24 semitones
        assert_eq!(part.midi_key_to_key(&memory, later, 24), 12);
    }

    /// The pedal holds and releases as CC64 does, and reset-all releases
    /// it too.
    #[test]
    fn the_pedal_releases_exactly_once() {
        let mut part = Part::new(0);
        assert!(!part.set_hold_pedal(true));
        assert!(part.hold_pedal());
        assert!(part.set_hold_pedal(false), "the release reports");
        assert!(!part.set_hold_pedal(false), "a second release is nothing");
        part.set_hold_pedal(true);
        assert!(part.reset_all_controllers());
        assert_eq!(part.expression(), 100);
        assert_eq!(part.pitch_bend(), 0);
    }

    /// The non-releasing count reports the part's first sound and its
    /// fall to silence, once each.
    #[test]
    fn the_part_reports_sounding_and_silence_once() {
        let mut part = Part::new(0);
        let play = StateChange {
            old: PolyStateKind::Inactive,
            new: PolyStateKind::Playing,
        };
        let release = StateChange {
            old: PolyStateKind::Playing,
            new: PolyStateKind::Releasing,
        };
        assert_eq!(
            part.poly_state_changed(play),
            Some(true),
            "first note sounds"
        );
        assert_eq!(part.poly_state_changed(play), None, "second is quiet news");
        assert_eq!(part.poly_state_changed(release), None, "one still plays");
        assert_eq!(part.poly_state_changed(release), Some(false), "silence");
        let mut rhythm = Part::new(RHYTHM_PART);
        assert_eq!(
            rhythm.poly_state_changed(play),
            None,
            "rhythm never reports"
        );
    }
}
