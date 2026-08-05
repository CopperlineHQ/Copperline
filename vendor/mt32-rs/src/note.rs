// SPDX-License-Identifier: LGPL-2.1-or-later

//! Notes and the caches they play from.
//!
//! A poly is one played note: a key, a velocity, and up to four partials,
//! walking Inactive -> Playing -> (Held) -> Releasing -> Inactive. The
//! reference wires polys, partials and parts together with pointers and
//! callbacks; here a poly is plain state addressed by id, and anything
//! that would have been a callback comes back to the caller as a value to
//! act on -- the engine owns every cascade.
//!
//! The patch cache is the note-start snapshot of a timbre: which partials
//! play, how each pair is structured, and a copy of the parameters as they
//! stood. The live parameter pointer the reference keeps is a
//! [`ParamSource`] here: where in memory the partial's parameters live, so
//! the envelopes read them as they stand now, which is how a SysEx write
//! reaches a note already sounding.

use crate::memory::TIMBRE_PARTIAL;

/// A partial slot in the engine's table.
pub type PartialId = usize;

/// How a structure number decodes: bit 1 set makes the first of the pair
/// PCM, bit 0 the second.
const PARTIAL_STRUCT: [u8; 13] = [0, 0, 2, 2, 1, 3, 3, 0, 3, 0, 2, 1, 3];

/// And how the pair combines: 0 mixes, 1 ring modulates with the master
/// mixed in, 2 ring modulates alone, 3 splits the pair to the sides.
const PARTIAL_MIX_STRUCT: [u8; 13] = [0, 1, 0, 1, 1, 0, 1, 3, 3, 2, 2, 2, 2];

/// Where a partial's live parameters sit in memory, resolved at each read
/// so writes reach sounding notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamSource {
    /// A melodic part's timbre temp: the part, and which partial.
    TimbreTemp { part: usize, partial: usize },
    /// A rhythm note's bank timbre: the absolute timbre, and which partial.
    Bank { timbre: usize, partial: usize },
}

/// The note-start snapshot of one partial of a timbre.
#[derive(Debug, Clone, Copy)]
pub struct PatchCache {
    pub play_partial: bool,
    pub pcm_partial: bool,
    /// The PCM wave number the parameters named.
    pub pcm: u8,
    pub waveform: u8,
    /// How this partial's pair combines (the mix table above).
    pub structure_mix: u32,
    /// 0 for the first of a pair, 1 for the second.
    pub structure_position: i32,
    /// The other slot of the pair.
    pub structure_pair: usize,
    /// Common to the whole timbre, stored redundantly as the firmware does.
    pub dirty: bool,
    pub partial_count: u32,
    pub sustain: bool,
    pub reverb: bool,
    /// The parameters as they stood when cached.
    pub src_partial: [u8; TIMBRE_PARTIAL],
    /// Where the live parameters are read from.
    pub param_source: ParamSource,
}

impl PatchCache {
    /// An empty, dirty slot: the state a part starts with, so the first
    /// note caches.
    pub fn empty(param_source: ParamSource) -> PatchCache {
        PatchCache {
            play_partial: false,
            pcm_partial: false,
            pcm: 0,
            waveform: 0,
            structure_mix: 0,
            structure_position: 0,
            structure_pair: 0,
            dirty: true,
            partial_count: 0,
            sustain: false,
            reverb: false,
            src_partial: [0; TIMBRE_PARTIAL],
            param_source,
        }
    }
}

/// Recompute a four-slot cache from a timbre's bytes, exactly as the
/// firmware caches at note-on: the mute mask picks the players, the two
/// structure numbers shape the pairs, and the parameters are snapshotted.
/// The caller backs playing partials up first, as the reference does.
pub fn cache_timbre(
    cache: &mut [PatchCache; 4],
    common: crate::param::TimbreCommon,
    partial_params: [&[u8]; 4],
    source: impl Fn(usize) -> ParamSource,
) {
    let mut partial_count = 0;
    for t in 0..4 {
        if (common.partial_mute() >> t) & 1 == 1 {
            cache[t].play_partial = true;
            partial_count += 1;
        } else {
            cache[t].play_partial = false;
            continue;
        }
        cache[t].src_partial.copy_from_slice(partial_params[t]);
        let param = crate::param::PartialParam(partial_params[t]);
        cache[t].pcm = param.pcm_wave();
        let structure = usize::from(if t < 2 {
            common.partial_structure12()
        } else {
            common.partial_structure34()
        });
        let pcm_bit = if t % 2 == 0 { 0x2 } else { 0x1 };
        cache[t].pcm_partial = PARTIAL_STRUCT[structure] & pcm_bit != 0;
        cache[t].structure_mix = u32::from(PARTIAL_MIX_STRUCT[structure]);
        cache[t].structure_position = (t % 2) as i32;
        cache[t].structure_pair = t ^ 1;
        cache[t].param_source = source(t);
        cache[t].waveform = param.waveform();
    }
    for slot in cache.iter_mut() {
        slot.dirty = false;
        slot.partial_count = partial_count;
        slot.sustain = common.no_sustain() == 0;
    }
}

/// Where a note is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolyStateKind {
    Inactive,
    Playing,
    /// The key came up under the hold pedal.
    Held,
    Releasing,
}

/// A poly's state moved; the part counts these to know when it fell
/// silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateChange {
    pub old: PolyStateKind,
    pub new: PolyStateKind,
}

/// What a note-off did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteOffOutcome {
    /// Nothing: already inactive or releasing.
    Ignored,
    /// The pedal holds it; the state moved to Held.
    NowHeld(StateChange),
    /// Decay begins: the caller starts every partial's release.
    StartDecay,
}

/// One played note.
#[derive(Debug, Clone)]
pub struct Poly {
    key: u32,
    velocity: u32,
    sustain: bool,
    state: PolyStateKind,
    partials: [Option<PartialId>; 4],
    active_partial_count: u32,
}

impl Poly {
    pub fn new() -> Poly {
        Poly {
            key: 255,
            velocity: 255,
            sustain: false,
            state: PolyStateKind::Inactive,
            partials: [None; 4],
            active_partial_count: 0,
        }
    }

    /// Start the note with its allocated partials. The caller hands in a
    /// fresh poly; what a state move means for the part comes back.
    pub fn reset(
        &mut self,
        key: u32,
        velocity: u32,
        sustain: bool,
        partials: [Option<PartialId>; 4],
    ) -> Option<StateChange> {
        debug_assert_eq!(self.state, PolyStateKind::Inactive, "polys start fresh");
        self.key = key;
        self.velocity = velocity;
        self.sustain = sustain;
        self.active_partial_count = 0;
        let mut change = None;
        self.partials = partials;
        for _ in partials.into_iter().flatten() {
            self.active_partial_count += 1;
            change = self.set_state(PolyStateKind::Playing).or(change);
        }
        change
    }

    /// The key came up. What follows depends on the pedal and the state.
    pub fn note_off(&mut self, pedal_held: bool) -> NoteOffOutcome {
        if matches!(
            self.state,
            PolyStateKind::Inactive | PolyStateKind::Releasing
        ) {
            return NoteOffOutcome::Ignored;
        }
        if pedal_held {
            if self.state == PolyStateKind::Held {
                return NoteOffOutcome::Ignored;
            }
            match self.set_state(PolyStateKind::Held) {
                Some(change) => NoteOffOutcome::NowHeld(change),
                None => NoteOffOutcome::Ignored,
            }
        } else {
            NoteOffOutcome::StartDecay
        }
    }

    /// The pedal released a held note: decay if it was being held.
    pub fn stop_pedal_hold(&mut self) -> bool {
        self.state == PolyStateKind::Held
    }

    /// Begin the release. The caller walks [`Self::partial_ids`] and starts
    /// every envelope's decay; the state move comes back for the part.
    pub fn start_decay(&mut self) -> Option<StateChange> {
        if matches!(
            self.state,
            PolyStateKind::Inactive | PolyStateKind::Releasing
        ) {
            return None;
        }
        self.set_state(PolyStateKind::Releasing)
    }

    /// Whether an abort may begin: the caller checks nothing else is
    /// aborting, then walks the partials with the amp's fast fall.
    pub fn can_abort(&self) -> bool {
        self.state != PolyStateKind::Inactive
    }

    /// One of this note's partials died. `true` once none are left, at
    /// which point the caller frees the poly and tells the part.
    pub fn partial_deactivated(&mut self, id: PartialId) -> (Option<StateChange>, bool) {
        for slot in self.partials.iter_mut() {
            if *slot == Some(id) {
                *slot = None;
                self.active_partial_count -= 1;
            }
        }
        if self.active_partial_count == 0 {
            (self.set_state(PolyStateKind::Inactive), true)
        } else {
            (None, false)
        }
    }

    fn set_state(&mut self, new: PolyStateKind) -> Option<StateChange> {
        if self.state == new {
            return None;
        }
        let change = StateChange {
            old: self.state,
            new,
        };
        self.state = new;
        Some(change)
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

    pub fn state(&self) -> PolyStateKind {
        self.state
    }

    pub fn active_partial_count(&self) -> u32 {
        self.active_partial_count
    }

    pub fn is_active(&self) -> bool {
        self.state != PolyStateKind::Inactive
    }

    /// The partial slots as they stand.
    pub fn partial_ids(&self) -> [Option<PartialId>; 4] {
        self.partials
    }
}

impl Default for Poly {
    fn default() -> Poly {
        Poly::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::param::TimbreCommon;

    /// The note's whole life: playing on reset, held under the pedal,
    /// releasing when it lifts, inactive when the last partial dies --
    /// with each transition reported exactly once.
    #[test]
    fn a_note_lives_and_dies_in_order() {
        let mut poly = Poly::new();
        assert!(!poly.is_active());
        let change = poly.reset(60, 100, true, [Some(3), Some(7), None, None]);
        assert_eq!(
            change,
            Some(StateChange {
                old: PolyStateKind::Inactive,
                new: PolyStateKind::Playing
            })
        );
        assert_eq!(poly.active_partial_count(), 2);

        // Note off under the pedal holds it; a second note off is nothing.
        let held = poly.note_off(true);
        assert!(matches!(held, NoteOffOutcome::NowHeld(_)));
        assert_eq!(poly.note_off(true), NoteOffOutcome::Ignored);

        // The pedal lifts: the caller is told to decay.
        assert!(poly.stop_pedal_hold());
        let change = poly.start_decay().expect("releasing is a move");
        assert_eq!(change.new, PolyStateKind::Releasing);
        assert_eq!(poly.note_off(false), NoteOffOutcome::Ignored);

        // The partials die one by one; only the last ends the note.
        let (change, done) = poly.partial_deactivated(3);
        assert!(change.is_none() && !done);
        let (change, done) = poly.partial_deactivated(7);
        assert!(done);
        assert_eq!(change.unwrap().new, PolyStateKind::Inactive);
        assert!(!poly.is_active());
    }

    /// The cache decodes the structure numbers as the firmware's tables
    /// do: structure 5 puts PCM on both of the first pair, ring structures
    /// mark their mix, and the mute mask decides who plays at all.
    #[test]
    fn the_cache_reads_the_structure_tables() {
        let mut common_bytes = [0u8; 14];
        common_bytes[10] = 5; // partials 1&2: structure 6 (PCM+PCM, mix 0)
        common_bytes[11] = 8; // partials 3&4: structure 9 (ring, mix 3)
        common_bytes[12] = 0b1011; // partial 3 muted
        common_bytes[13] = 1; // no sustain
        let p0 = [0u8; TIMBRE_PARTIAL];
        let mut p1 = [0u8; TIMBRE_PARTIAL];
        p1[4] = 1; // sawtooth
        p1[5] = 42; // pcm wave
        let params = [&p0[..], &p1[..], &p0[..], &p0[..]];
        let mut cache = [PatchCache::empty(ParamSource::TimbreTemp {
            part: 0,
            partial: 0,
        }); 4];
        cache_timbre(&mut cache, TimbreCommon(&common_bytes), params, |t| {
            ParamSource::TimbreTemp {
                part: 2,
                partial: t,
            }
        });
        assert!(cache[0].play_partial && cache[1].play_partial && cache[3].play_partial);
        assert!(!cache[2].play_partial, "muted");
        assert_eq!(cache[0].partial_count, 3);
        assert!(
            cache[0].pcm_partial && cache[1].pcm_partial,
            "structure 5 is PCM twice"
        );
        assert_eq!(cache[1].pcm, 42);
        assert_eq!(cache[1].waveform, 1);
        assert_eq!(cache[3].structure_mix, 3, "structure 8 splits the pair");
        assert_eq!(cache[3].structure_pair, 2);
        assert_eq!(cache[3].structure_position, 1);
        assert!(!cache[0].sustain, "no-sustain timbre");
        assert!(!cache[0].dirty);
        assert_eq!(
            cache[3].param_source,
            ParamSource::TimbreTemp {
                part: 2,
                partial: 3
            }
        );
    }
}
