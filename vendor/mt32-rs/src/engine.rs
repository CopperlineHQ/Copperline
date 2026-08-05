// SPDX-License-Identifier: LGPL-2.1-or-later

//! The engine: everything wired together and rendering.
//!
//! This is the arena the reference builds from pointers: the engine owns
//! the memory, the parts, the polys and the partials, and every cascade
//! the reference runs through callbacks is a function here working by id.
//! Rendering follows the reference's shape exactly -- chunks of up to 4096
//! samples, one sample at a time while a poly aborts, each partial
//! producing its whole chunk before the next so the jitter generator is
//! consumed in the same order -- through the NICE DAC transform and the
//! digital-only analog combine.
//!
//! The wet stream runs through the Boss reverb chip, selected and
//! re-aimed by the system area's reverb bytes exactly as the firmware
//! does it: a mode change powers a fresh chip, a time or level change
//! re-aims the running one, and zero time with zero level switches the
//! chip out.

use crate::analog::{Analog, AnalogMode, Streams};
use crate::display::Display;
use crate::jitter::Jitter;
use crate::la32::wave::Pair;
use crate::layout::{layout_for, Layout, Quirks};
use crate::memory::{Memory, Touched};
use crate::note::{NoteOffOutcome, Poly, PolyStateKind};
use crate::param::{PartialParam, PatchTemp, RhythmTemp};
use crate::part::{Part, PART_COUNT, RHYTHM_PART};
use crate::partial::{LiveValues, NiceOptions, Partial, StartArgs};
use crate::pcm::Wave;
use crate::reverb::Reverb;
use crate::rom;
use crate::sysex;
use crate::tables::Tables;

impl crate::midi::Sink for Engine {
    fn short_message(&mut self, message: u32) {
        self.play_msg(message);
    }

    fn sysex(&mut self, frame: &[u8]) {
        self.play_sysex(frame);
    }
}

/// One message waiting its turn between the host and the parts.
enum Queued {
    Short(u32),
    Sysex(Vec<u8>),
}

/// The chip's voice count, and the poly pool that matches it.
pub const PARTIAL_COUNT: usize = 32;

/// The render chunk bound, as the reference sizes its buffers.
const MAX_SAMPLES_PER_RUN: usize = 4096;

/// The whole module.
pub struct Engine {
    quirks: Quirks,
    layout: Layout,
    /// The control image, kept whole: reset re-powers memory from it.
    control: Vec<u8>,
    tables: Tables,
    jitter: Jitter,
    memory: Memory,
    pcm: Vec<i16>,
    waves: Vec<Wave>,
    display: Display,
    parts: Vec<Part>,
    partials: Vec<Partial>,
    /// Free partial slots; the top of the stack allocates first, seeded
    /// so slot zero is the first voice used.
    inactive_partials: Vec<usize>,
    polys: Vec<Poly>,
    free_polys: Vec<usize>,
    /// The partial reserve per part, from the system area.
    reserved: [u32; PART_COUNT],
    /// Which part each MIDI channel drives, 0xFF ending the list.
    chantable: [[u8; PART_COUNT]; 16],
    reverb: Option<Reverb>,
    /// Messages accepted and not yet fully played. A short message that
    /// starts a voice abort stays at the head and is retried as the
    /// abort renders out, exactly as the reference's event queue does;
    /// everything behind it waits its turn.
    queue: std::collections::VecDeque<Queued>,
    /// The OUT jack: what the module has to say back, drained by the
    /// host. Only read requests put anything here.
    midi_out: Vec<u8>,
    /// The reference holds everything still until a short message has
    /// actually reached a part; the gate is observable through the
    /// reverb store it leaves frozen.
    activated: bool,
    aborting_poly: Option<usize>,
    aborting_part_ix: usize,
    rendered_sample_count: u32,
    nice: NiceOptions,
    master_tune_pitch_delta: i32,
    /// The analogue stage between the DAC and the jacks, and the stream
    /// scratch the digital half fills for it.
    analog: Analog,
    streams: Streams,
}

impl Engine {
    /// Open the module on a control and PCM ROM pair, with no analogue
    /// circuit modelled: the LA32's stream as it stands, at the native
    /// rate.
    pub fn open(control_image: &[u8], pcm_image: &[u8]) -> Result<Engine, String> {
        Engine::open_with_analog(control_image, pcm_image, AnalogMode::DigitalOnly)
    }

    /// Open the module with one of the analogue output models; the rate
    /// [Self::output_sample_rate] reports depends on it.
    pub fn open_with_analog(
        control_image: &[u8],
        pcm_image: &[u8],
        mode: AnalogMode,
    ) -> Result<Engine, String> {
        let control_info =
            rom::identify(control_image).ok_or_else(|| "unknown control ROM".to_string())?;
        let pcm_info = rom::identify(pcm_image).ok_or_else(|| "unknown PCM ROM".to_string())?;
        if pcm_info.kind != rom::Kind::Pcm || control_info.kind != rom::Kind::Control {
            return Err("the pair is not a control ROM and a PCM ROM".to_string());
        }
        let layout = *layout_for(control_info).ok_or_else(|| "no layout".to_string())?;
        let memory = Memory::power_on(control_image, &layout)?;
        let pcm = crate::pcm::decode(pcm_image);
        let waves = crate::pcm::waves(control_image, &layout, pcm.len())?;
        let display = Display::power_on(control_image, &layout);
        let mut engine = Engine {
            quirks: layout.quirks,
            layout,
            control: control_image.to_vec(),
            tables: Tables::new(),
            jitter: Jitter::new(),
            memory,
            pcm,
            waves,
            display,
            parts: (0..PART_COUNT).map(Part::new).collect(),
            partials: (0..PARTIAL_COUNT)
                .map(|_| Partial::new(&layout.quirks))
                .collect(),
            inactive_partials: (0..PARTIAL_COUNT).rev().collect(),
            polys: (0..PARTIAL_COUNT).map(|_| Poly::new()).collect(),
            free_polys: (0..PARTIAL_COUNT).rev().collect(),
            reserved: [0; PART_COUNT],
            chantable: [[0xFF; PART_COUNT]; 16],
            reverb: None,
            queue: std::collections::VecDeque::new(),
            midi_out: Vec::new(),
            activated: false,
            aborting_poly: None,
            aborting_part_ix: 0,
            rendered_sample_count: 0,
            nice: NiceOptions::default(),
            analog: Analog::new(mode, layout.quirks.old_mt32_analog_lpf),
            streams: Streams::default(),
            master_tune_pitch_delta: 0,
        };
        engine.refresh_reserve();
        engine.refresh_chan_assign(0, 8, false);
        engine.refresh_reverb();
        for part in 0..PART_COUNT {
            if part == RHYTHM_PART {
                let count = usize::from(engine.layout.rhythm_settings_count);
                engine.parts[part].refresh_rhythm(&engine.memory, count);
            } else {
                engine.parts[part].refresh(&engine.memory);
            }
        }
        // The 440 Hz bug: the reset routine zeroes the delta the power-on
        // master tune should have set, on every supported ROM.
        engine.master_tune_pitch_delta = 0;
        Ok(engine)
    }

    pub fn display(&mut self) -> &mut Display {
        &mut self.display
    }

    pub fn memory(&mut self) -> &mut Memory {
        &mut self.memory
    }

    /// The rate frames leave [Self::render] at: the native 32 kHz, or
    /// the analogue model's own.
    pub fn output_sample_rate(&self) -> u32 {
        self.analog.output_sample_rate()
    }

    pub fn rendered_sample_count(&self) -> u32 {
        self.rendered_sample_count
    }

    pub fn is_aborting_poly(&self) -> bool {
        self.aborting_poly.is_some()
    }

    /// Drain the OUT jack: the replies accumulated since the last call,
    /// in the order they were made.
    pub fn take_midi_out(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.midi_out)
    }

    /// How many of the chip's thirty-two voices are sounding, for a host
    /// showing voice usage.
    pub fn active_partial_count(&self) -> usize {
        PARTIAL_COUNT - self.inactive_partials.len()
    }

    fn refresh_reserve(&mut self) {
        let mut reserved = [0u32; PART_COUNT];
        for (i, slot) in reserved.iter_mut().enumerate() {
            *slot = u32::from(self.memory.system()[4 + i]);
        }
        self.reserved = reserved;
    }

    /// Rebuild the channel table, silencing the parts whose assignment
    /// moved, exactly as the firmware's refresh does.
    fn refresh_chan_assign(&mut self, first_part: usize, last_part: usize, silence: bool) {
        self.chantable = [[0xFF; PART_COUNT]; 16];
        for part in 0..PART_COUNT {
            if silence && (first_part..=last_part).contains(&part) {
                self.all_sound_off(part);
                if self.parts[part].reset_all_controllers() {
                    self.stop_pedal_hold(part);
                }
            }
            let chan = self.memory.system()[13 + part];
            if chan > 15 {
                continue;
            }
            let row = &mut self.chantable[usize::from(chan)];
            if let Some(slot) = row.iter_mut().find(|s| **s > 8) {
                *slot = part as u8;
            }
        }
    }

    // ------------------------------------------------------------------
    // MIDI in.

    /// One short message, the low byte the status. Played now if the
    /// module is free; a message that runs into a voice abort finishes
    /// as the abort renders out, and anything sent meanwhile queues
    /// behind it in order.
    pub fn play_msg(&mut self, msg: u32) {
        self.activated = true;
        self.queue.push_back(Queued::Short(msg));
    }

    /// The head-of-queue walk: plays as much of the message as the
    /// voices allow, leaving the abort books set if it must pause.
    fn process_msg(&mut self, msg: u32) {
        let command = ((msg & 0xF0) >> 4) as u8;
        if command == 0x0F || command < 8 {
            return;
        }
        let chan = (msg & 0x0F) as usize;
        let data1 = ((msg & 0xFF00) >> 8) as u8;
        let data2 = if command & 0x0E == 0x0C {
            0
        } else {
            ((msg & 0xFF_0000) >> 16) as u8
        };
        if data1 > 127 || data2 > 127 {
            return;
        }
        if self.chantable[chan][0] > 8 {
            return;
        }
        let start = self.aborting_part_ix;
        for i in start..PART_COUNT {
            let part = self.chantable[chan][i];
            if part > 8 {
                break;
            }
            self.play_unpacked(usize::from(part), command, data1, data2);
            if self.is_aborting_poly() {
                self.aborting_part_ix = i;
                break;
            } else if self.aborting_part_ix != 0 {
                self.aborting_part_ix = 0;
            }
        }
    }

    fn play_unpacked(&mut self, part: usize, command: u8, data1: u8, data2: u8) {
        self.activated = true;
        match command {
            0x8 => self.note_off(part, u32::from(data1)),
            0x9 => {
                if data2 == 0 {
                    self.note_off(part, u32::from(data1));
                } else if self.parts[part].volume_override() > 0 {
                    self.note_on(part, u32::from(data1), u32::from(data2));
                }
            }
            0xB => {
                match data1 {
                    0x01 => self.parts[part].set_modulation(u32::from(data2)),
                    0x06 => {
                        self.parts[part].set_data_entry_msb(&mut self.memory, data2);
                    }
                    0x07 => self.parts[part].set_volume(&mut self.memory, u32::from(data2)),
                    0x0A => {
                        self.parts[part].set_pan(&mut self.memory, u32::from(data2), &self.quirks)
                    }
                    0x0B => self.parts[part].set_expression(u32::from(data2)),
                    0x40 => {
                        if self.parts[part].set_hold_pedal(data2 >= 64) {
                            self.stop_pedal_hold(part);
                        }
                    }
                    0x62 | 0x63 => self.parts[part].set_nrpn(),
                    0x64 => self.parts[part].set_rpn_lsb(data2),
                    0x65 => self.parts[part].set_rpn_msb(data2),
                    0x79 => {
                        if self.parts[part].reset_all_controllers() {
                            self.stop_pedal_hold(part);
                        }
                    }
                    0x7B => self.all_notes_off(part),
                    0x7C..=0x7F => {
                        self.parts[part].set_hold_pedal(false);
                        self.all_notes_off(part);
                    }
                    _ => return,
                }
                self.display.midi_message_played(self.rendered_sample_count);
            }
            0xC => {
                self.set_program(part, usize::from(data1));
                if part < RHYTHM_PART {
                    self.display.midi_message_played(self.rendered_sample_count);
                    let group = self.sound_group_name(part);
                    let timbre = *self.parts[part].current_instr();
                    self.display.program_changed(
                        self.rendered_sample_count,
                        part as u8,
                        group,
                        timbre,
                    );
                }
            }
            0xE => {
                self.parts[part].set_bend((u32::from(data2) << 7) | u32::from(data1));
                self.display.midi_message_played(self.rendered_sample_count);
            }
            _ => {}
        }
    }

    /// The sound group beside a program change notice. The firmware's
    /// group lookup is not yet ported; the line is cosmetic and joins
    /// with the full display differential. Space-filled, delimiter and
    /// all, until then.
    fn sound_group_name(&self, _part: usize) -> [u8; 8] {
        let mut name = [b' '; 8];
        name[7] = b'|';
        name
    }

    /// A SysEx message: through the memory model, with every consequence
    /// the firmware runs -- refreshes, resets, the display. Queued in
    /// order behind any short message still finishing its abort.
    pub fn play_sysex(&mut self, message: &[u8]) {
        self.activated = true;
        self.queue.push_back(Queued::Sysex(message.to_vec()));
    }

    /// A SysEx message applied at once, ahead of anything queued: the
    /// path a host's own controls use, as the reference's immediate
    /// entry. Does not wake the module -- a panel edit is not traffic.
    pub fn play_sysex_now(&mut self, message: &[u8]) {
        self.process_sysex(message);
    }

    /// One queued message played, if one is waiting: a short message
    /// that starts a voice abort stays at the head, to be walked again
    /// -- from its resume point -- once rendering completes the abort.
    fn play_one_queued(&mut self) {
        match self.queue.front() {
            None => {}
            Some(Queued::Short(msg)) => {
                let msg = *msg;
                self.process_msg(msg);
                if !self.is_aborting_poly() {
                    self.queue.pop_front();
                }
            }
            Some(Queued::Sysex(_)) => {
                let Some(Queued::Sysex(frame)) = self.queue.pop_front() else {
                    unreachable!("the head was just a SysEx");
                };
                self.process_sysex(&frame);
            }
        }
    }

    fn process_sysex(&mut self, message: &[u8]) {
        let outcome = sysex::play(&mut self.memory, message);
        match outcome {
            sysex::Outcome::Written(touched) => {
                self.display.midi_message_played(self.rendered_sample_count);
                for touch in touched {
                    self.apply_touch(touch);
                }
            }
            sysex::Outcome::DisplayControl(bytes) => {
                self.display.midi_message_played(self.rendered_sample_count);
                self.display.display_control(&bytes[1..]);
            }
            sysex::Outcome::ChecksumError => {
                self.display.checksum_error(self.rendered_sample_count);
            }
            sysex::Outcome::Reset => {
                if !self.quirks.old_mt32_display_features {
                    self.display.midi_message_played(self.rendered_sample_count);
                }
                self.reset();
            }
            sysex::Outcome::ReadRequest { printed_addr, len } => {
                let reply = crate::reply::answer(&mut self.memory, printed_addr, len);
                self.midi_out.extend(reply);
            }
            sysex::Outcome::Ignored => {}
        }
    }

    fn apply_touch(&mut self, touch: Touched) {
        match touch {
            Touched::Ram {
                region,
                offset,
                len,
            } => self.refresh_after_write(region, offset, len),
            Touched::Display { offset, data } => {
                let len = data.len().min(crate::display::LCD_TEXT_SIZE);
                self.display.custom_message(&data[..len], offset);
            }
            Touched::Reset => self.reset(),
        }
    }

    /// The refreshes a region write runs. The system area follows the
    /// firmware's offset windows exactly -- rebuilding the channel table
    /// silences the touched parts, so it must not run for a write that
    /// never reached those bytes. The other regions refresh coarsely,
    /// which over-refreshes but is idempotent.
    fn refresh_after_write(&mut self, region: crate::memory::Region, offset: usize, len: usize) {
        use crate::memory::Region;
        match region {
            Region::System => {
                if offset == 0 {
                    let tune = self.memory.system()[0];
                    self.master_tune_pitch_delta = ((i32::from(tune) - 64) * 171) >> 6;
                }
                if offset <= 3 && offset + len > 1 {
                    self.refresh_reverb();
                }
                if offset <= 12 && offset + len > 4 {
                    self.refresh_reserve();
                }
                if offset <= 21 && offset + len > 13 {
                    let first = offset.saturating_sub(13);
                    let last = (offset + len - 13).min(8);
                    self.refresh_chan_assign(first, last, true);
                }
                if offset <= 22 && offset + len > 22 {
                    self.display.master_volume_changed();
                }
            }
            Region::PatchTemp => {
                for part in 0..PART_COUNT {
                    if part == RHYTHM_PART {
                        let count = usize::from(self.layout.rhythm_settings_count);
                        self.parts[part].refresh_rhythm(&self.memory, count);
                    } else {
                        self.parts[part].refresh(&self.memory);
                    }
                }
            }
            Region::RhythmTemp => {
                let count = usize::from(self.layout.rhythm_settings_count);
                self.parts[RHYTHM_PART].refresh_rhythm(&self.memory, count);
            }
            Region::TimbreTemp => {
                for part in 0..RHYTHM_PART {
                    self.parts[part].refresh(&self.memory);
                }
            }
            Region::Timbres => {
                for part in 0..PART_COUNT {
                    for timbre in 128..192 {
                        self.parts[part].refresh_timbre(&self.memory, timbre);
                    }
                }
            }
            _ => {}
        }
    }

    /// The 0x7F reset: every voice dies on the spot, memory goes back to
    /// power-on and the books follow. The display keeps running, and so
    /// does the reverb chip -- unless the mode byte comes back different,
    /// its tail rings straight through the reset.
    pub fn reset(&mut self) {
        for id in 0..PARTIAL_COUNT {
            self.deactivate_partial(id);
        }
        self.memory =
            Memory::power_on(&self.control, &self.layout).expect("the image opened once already");
        self.parts = (0..PART_COUNT).map(Part::new).collect();
        self.polys = (0..PARTIAL_COUNT).map(|_| Poly::new()).collect();
        self.free_polys = (0..PARTIAL_COUNT).rev().collect();
        self.aborting_poly = None;
        self.aborting_part_ix = 0;
        self.master_tune_pitch_delta = 0;
        self.refresh_reserve();
        self.refresh_chan_assign(0, 8, false);
        self.refresh_reverb();
        for part in 0..PART_COUNT {
            if part == RHYTHM_PART {
                let count = usize::from(self.layout.rhythm_settings_count);
                self.parts[part].refresh_rhythm(&self.memory, count);
            } else {
                self.parts[part].refresh(&self.memory);
            }
        }
        // The reference re-evaluates its gate here: only a ringing
        // reverb store keeps the machinery running.
        self.activated = self.reverb.as_ref().is_some_and(Reverb::is_active);
    }

    // ------------------------------------------------------------------
    // Notes.

    fn note_on(&mut self, part: usize, midi_key: u32, velocity: u32) {
        if part == RHYTHM_PART {
            self.rhythm_note_on(midi_key, velocity);
            return;
        }
        let key = self.parts[part].midi_key_to_key(&self.memory, &self.quirks, midi_key);
        if self.parts[part].patch_cache()[0].dirty {
            self.backup_part_caches(part);
            self.parts[part].cache_timbre_from_temp(&self.memory);
        }
        self.play_poly(part, None, midi_key, key, velocity);
    }

    fn rhythm_note_on(&mut self, midi_key: u32, velocity: u32) {
        if !(24..=108).contains(&midi_key) {
            return;
        }
        self.display.rhythm_note_played(self.rendered_sample_count);
        let mut key = midi_key;
        let drum = (key - 24) as usize;
        let drum_timbre = u32::from(RhythmTemp(self.memory.rhythm_temp(drum)).timbre());
        let drum_timbre_count = 64 + u32::from(self.layout.timbre_r_count);
        if drum_timbre == 127 || drum_timbre >= drum_timbre_count {
            return;
        }
        if drum_timbre == 64 + 6 {
            self.note_off(RHYTHM_PART, 0);
            key = 1;
        } else if drum_timbre == 64 + 7 {
            self.note_off(RHYTHM_PART, 0);
            key = 0;
        }
        let abs_timbre = drum_timbre as usize + 128;
        if self.parts[RHYTHM_PART].drum_cache(drum)[0].dirty {
            self.backup_part_caches(RHYTHM_PART);
            self.parts[RHYTHM_PART].cache_drum_timbre(&self.memory, drum, abs_timbre);
        }
        self.play_poly(RHYTHM_PART, Some(drum), midi_key, key, velocity);
    }

    /// The cache is about to be rewritten: playing partials keep copies
    /// already, so there is nothing to do -- the reference's backup dance
    /// is the pointer's problem, not the copy's.
    fn backup_part_caches(&mut self, _part: usize) {}

    fn play_poly(
        &mut self,
        part: usize,
        drum: Option<usize>,
        _midi_key: u32,
        key: u32,
        velocity: u32,
    ) {
        let cache = match drum {
            Some(d) => *self.parts[part].drum_cache(d),
            None => *self.parts[part].patch_cache(),
        };
        let needed = cache[0].partial_count;
        if needed == 0 {
            return;
        }
        let assign_mode = PatchTemp(self.memory.patch_temp(part)).assign_mode();
        if assign_mode & 2 == 0 {
            self.abort_first_poly_with_key(part, key);
            if self.is_aborting_poly() {
                return;
            }
        }
        if !self.free_partials(needed, part) {
            return;
        }
        if self.is_aborting_poly() {
            return;
        }
        let Some(poly_id) = self.free_polys.pop() else {
            return;
        };
        self.parts[part].add_poly(poly_id, assign_mode & 1 != 0);

        let mut allocated: [Option<usize>; 4] = [None; 4];
        for (slot, entry) in cache.iter().enumerate() {
            if entry.play_partial {
                allocated[slot] = self.alloc_partial(part);
                self.parts[part].note_partials_started(1);
            }
        }
        if let Some(change) = self.polys[poly_id].reset(key, velocity, cache[0].sustain, allocated)
        {
            if let Some(sounding) = self.parts[part].poly_state_changed(change) {
                self.display.voice_part_state_changed(part, sounding);
            }
        }

        let pan_setting = match drum {
            Some(d) => RhythmTemp(self.memory.rhythm_temp(d)).panpot(),
            None => PatchTemp(self.memory.patch_temp(part)).panpot(),
        };
        for slot in 0..4 {
            let Some(id) = allocated[slot] else { continue };
            let live = self.live_values(part, drum);
            let args = StartArgs {
                cache: &cache[slot],
                pan_setting,
                key,
                velocity,
                sustain: cache[0].sustain,
                partial_index: id,
                extended_pcm: self.layout.pcm_count > 128,
                options: self.nice,
                rhythm_key: drum,
            };
            let pair = allocated[cache[slot].structure_pair];
            let setup = self.partials[id].start(
                &self.tables,
                &self.quirks,
                args,
                poly_id,
                pair,
                &self.waves,
                &live,
            );
            if setup.slave_side {
                let master = pair.expect("a slave has its master");
                let (a, b) = two(&mut self.partials, master, id);
                let _ = b;
                if let Some((addr, len, looped)) = setup.pcm {
                    a.la32.init_pcm(Pair::Slave, addr, len, looped);
                } else if let Some((sawtooth, pulse_width, resonance)) = setup.synth {
                    a.la32
                        .init_synth(&self.tables, Pair::Slave, sawtooth, pulse_width, resonance);
                }
            } else {
                let tables = &self.tables;
                self.partials[id].apply_own_la32_setup(tables, setup);
            }
        }
    }

    fn note_off(&mut self, part: usize, midi_key: u32) {
        let key = if part == RHYTHM_PART {
            midi_key
        } else {
            self.parts[part].midi_key_to_key(&self.memory, &self.quirks, midi_key)
        };
        self.stop_note(part, key);
    }

    fn stop_note(&mut self, part: usize, key: u32) {
        let hold = self.parts[part].hold_pedal();
        let poly_ids: Vec<usize> = self.parts[part].active_polys().to_vec();
        for poly_id in poly_ids {
            let poly = &self.polys[poly_id];
            if poly.key() == key && (poly.can_sustain() || key == 0) {
                let outcome = self.polys[poly_id].note_off(hold && key != 0);
                match outcome {
                    NoteOffOutcome::Ignored => continue,
                    NoteOffOutcome::NowHeld(change) => {
                        if let Some(sounding) = self.parts[part].poly_state_changed(change) {
                            self.display.voice_part_state_changed(part, sounding);
                        }
                        break;
                    }
                    NoteOffOutcome::StartDecay => {
                        self.start_poly_decay(part, poly_id);
                        break;
                    }
                }
            }
        }
    }

    fn start_poly_decay(&mut self, part: usize, poly_id: usize) {
        let Some(change) = self.polys[poly_id].start_decay() else {
            return;
        };
        if let Some(sounding) = self.parts[part].poly_state_changed(change) {
            self.display.voice_part_state_changed(part, sounding);
        }
        for id in self.polys[poly_id].partial_ids().into_iter().flatten() {
            let source = self.partials[id].param_source();
            if let Some(source) = source {
                let param_bytes = self.memory.partial_param(source).to_vec();
                self.partials[id].start_decay_all(&self.tables, PartialParam(&param_bytes));
            }
        }
    }

    fn stop_pedal_hold(&mut self, part: usize) {
        let poly_ids: Vec<usize> = self.parts[part].active_polys().to_vec();
        for poly_id in poly_ids {
            if self.polys[poly_id].stop_pedal_hold() {
                self.start_poly_decay(part, poly_id);
            }
        }
    }

    fn all_notes_off(&mut self, part: usize) {
        let hold = self.parts[part].hold_pedal();
        let poly_ids: Vec<usize> = self.parts[part].active_polys().to_vec();
        for poly_id in poly_ids {
            match self.polys[poly_id].note_off(hold) {
                NoteOffOutcome::NowHeld(change) => {
                    if let Some(sounding) = self.parts[part].poly_state_changed(change) {
                        self.display.voice_part_state_changed(part, sounding);
                    }
                }
                NoteOffOutcome::StartDecay => self.start_poly_decay(part, poly_id),
                NoteOffOutcome::Ignored => {}
            }
        }
    }

    fn all_sound_off(&mut self, part: usize) {
        let poly_ids: Vec<usize> = self.parts[part].active_polys().to_vec();
        for poly_id in poly_ids {
            self.start_poly_decay(part, poly_id);
        }
    }

    fn set_program(&mut self, part: usize, patch_num: usize) {
        if part == RHYTHM_PART {
            return;
        }
        let new_patch: Vec<u8> = self.memory.patch(patch_num).to_vec();
        self.memory.patch_temp_mut(part)[..8].copy_from_slice(&new_patch);
        // resetTimbre: the pedal lets go, everything stops, and the bank
        // timbre is copied into the temp.
        self.parts[part].set_hold_pedal(false);
        self.all_sound_off(part);
        let abs = self.parts[part].abs_timbre_num(&self.memory);
        let timbre: Vec<u8> = self.memory.bank_timbre(abs).to_vec();
        self.memory.timbre_temp_mut(part).copy_from_slice(&timbre);
        self.backup_part_caches(part);
        self.parts[part].refresh(&self.memory);
    }

    // ------------------------------------------------------------------
    // The allocator.

    fn alloc_partial(&mut self, part: usize) -> Option<usize> {
        let id = self.inactive_partials.pop()?;
        self.partials[id].activate(part);
        Some(id)
    }

    /// A partial died: the whole cascade the reference runs through
    /// three classes, in order.
    fn deactivate_partial(&mut self, id: usize) {
        if !self.partials[id].is_active() {
            return;
        }
        let owner = self.partials[id].owner_part();
        let poly = self.partials[id].poly();
        let pair = self.partials[id].pair();
        let slave_side = self.partials[id].is_ring_modulating_slave();
        let had_slave = self.partials[id].has_ring_modulating_slave();
        self.partials[id].clear_for_deactivation();
        self.inactive_partials.push(id);
        if let Some(poly_id) = poly {
            let (change, freed) = self.polys[poly_id].partial_deactivated(id);
            if let Some(part) = owner {
                if let Some(change) = change {
                    if let Some(sounding) = self.parts[part].poly_state_changed(change) {
                        self.display.voice_part_state_changed(part, sounding);
                    }
                }
                self.parts[part].partial_deactivated();
                if freed {
                    self.parts[part].remove_poly(poly_id);
                    self.free_polys.push(poly_id);
                    if self.aborting_poly == Some(poly_id) {
                        self.aborting_poly = None;
                    }
                }
            }
        }
        if let Some(pair_id) = pair {
            if slave_side {
                self.partials[pair_id].la32.deactivate(Pair::Slave);
            } else {
                self.partials[id].la32.deactivate(Pair::Master);
                if had_slave {
                    self.deactivate_partial(pair_id);
                    self.partials[id].set_pair(None);
                }
            }
            self.partials[pair_id].set_pair(None);
        } else {
            self.partials[id].la32.deactivate(Pair::Master);
        }
    }

    fn free_partial_count(&self) -> u32 {
        self.inactive_partials.len() as u32
    }

    fn active_partial_count_of(&self, part: usize) -> u32 {
        self.parts[part].active_partial_count()
    }

    fn active_non_releasing_partial_count(&self, part: usize) -> u32 {
        self.parts[part]
            .active_polys()
            .iter()
            .filter(|&&p| self.polys[p].state() != PolyStateKind::Releasing)
            .map(|&p| self.polys[p].active_partial_count())
            .sum()
    }

    fn start_poly_abort(&mut self, part: usize, poly_id: usize) -> bool {
        if !self.polys[poly_id].can_abort() || self.is_aborting_poly() {
            return false;
        }
        let mut any = false;
        for id in self.polys[poly_id].partial_ids().into_iter().flatten() {
            self.partials[id].start_abort(&self.tables);
            self.aborting_poly = Some(poly_id);
            any = true;
        }
        let _ = part;
        any
    }

    fn abort_first_poly_with_key(&mut self, part: usize, key: u32) -> bool {
        let candidate = self.parts[part]
            .active_polys()
            .iter()
            .copied()
            .find(|&p| self.polys[p].key() == key);
        match candidate {
            Some(poly_id) => self.start_poly_abort(part, poly_id),
            None => false,
        }
    }

    fn abort_first_poly_in_state(&mut self, part: usize, state: PolyStateKind) -> bool {
        let candidate = self.parts[part]
            .active_polys()
            .iter()
            .copied()
            .find(|&p| self.polys[p].state() == state);
        match candidate {
            Some(poly_id) => self.start_poly_abort(part, poly_id),
            None => false,
        }
    }

    fn abort_first_poly_prefer_held(&mut self, part: usize) -> bool {
        if self.abort_first_poly_in_state(part, PolyStateKind::Held) {
            return true;
        }
        let first = self.parts[part].active_polys().first().copied();
        match first {
            Some(poly_id) => self.start_poly_abort(part, poly_id),
            None => false,
        }
    }

    fn abort_on_part_releasing_then_held(&mut self, part: usize) -> bool {
        if self.abort_first_poly_in_state(part, PolyStateKind::Releasing) {
            return true;
        }
        self.abort_first_poly_prefer_held(part)
    }

    /// Walk parts 7 down to `min_part` (-1 standing for the rhythm part
    /// last), aborting on the first over its reserve.
    fn abort_where_reserve_exceeded(
        &mut self,
        min_part: i32,
        prefer: fn(&mut Engine, usize) -> bool,
    ) -> bool {
        let mut part_num = 7i32;
        while part_num >= min_part {
            let use_part = if part_num == -1 { 8 } else { part_num as usize };
            if self.active_partial_count_of(use_part) > self.reserved[use_part]
                && prefer(self, use_part)
            {
                return true;
            }
            part_num -= 1;
        }
        false
    }

    /// Make room for `needed` partials for `part`: the firmware's own
    /// stealing, in its two generations.
    fn free_partials(&mut self, needed: u32, part: usize) -> bool {
        if self.quirks.new_gen_note_cancellation {
            return self.free_partials_new_gen(needed, part);
        }
        while !self.is_aborting_poly() && self.free_partial_count() < needed {
            if self.active_non_releasing_partial_count(part) + needed > self.reserved[part] {
                let assign = PatchTemp(self.memory.patch_temp(part)).assign_mode();
                if assign & 1 != 0 {
                    return false;
                }
                if needed <= self.reserved[part] {
                    self.abort_on_part_releasing_then_held(part);
                    continue;
                }
                let min = if part < 8 { part as i32 } else { 0 };
                if self.abort_where_reserve_exceeded(min, |e, p| {
                    e.abort_on_part_releasing_then_held(p)
                }) {
                    continue;
                }
                if self.active_partial_count_of(8) > self.reserved[8]
                    && self.abort_on_part_releasing_then_held(8)
                {
                    continue;
                }
                return false;
            }
            if self.abort_where_reserve_exceeded(0, |e, p| e.abort_on_part_releasing_then_held(p)) {
                continue;
            }
            if self.active_partial_count_of(8) > self.reserved[8]
                && self.abort_on_part_releasing_then_held(8)
            {
                continue;
            }
            if self.abort_on_part_releasing_then_held(part) {
                continue;
            }
            return false;
        }
        true
    }

    fn free_partials_new_gen(&mut self, needed: u32, part: usize) -> bool {
        if needed == 0 || self.free_partial_count() >= needed {
            return true;
        }
        loop {
            if !self.abort_where_reserve_exceeded(0, |e, p| {
                e.abort_first_poly_in_state(p, PolyStateKind::Releasing)
            }) {
                break;
            }
            if self.is_aborting_poly() || self.free_partial_count() >= needed {
                return true;
            }
        }
        if self.active_non_releasing_partial_count(part) + needed > self.reserved[part] {
            let assign = PatchTemp(self.memory.patch_temp(part)).assign_mode();
            if assign & 1 != 0 {
                return false;
            }
            loop {
                if !self.abort_where_reserve_exceeded(part as i32, |e, p| {
                    e.abort_first_poly_prefer_held(p)
                }) {
                    break;
                }
                if self.is_aborting_poly() || self.free_partial_count() >= needed {
                    return true;
                }
            }
            if needed > self.reserved[part] {
                return false;
            }
        } else {
            loop {
                if !self.abort_where_reserve_exceeded(-1, |e, p| e.abort_first_poly_prefer_held(p))
                {
                    break;
                }
                if self.is_aborting_poly() || self.free_partial_count() >= needed {
                    return true;
                }
            }
        }
        loop {
            if !self.abort_first_poly_prefer_held(part) {
                break;
            }
            if self.is_aborting_poly() || self.free_partial_count() >= needed {
                return true;
            }
        }
        false
    }

    // ------------------------------------------------------------------
    // Rendering.

    fn live_values(&self, part: usize, drum: Option<usize>) -> LiveValues {
        let patch = PatchTemp(self.memory.patch_temp(part));
        LiveValues {
            part_volume: self.parts[part].volume(&self.memory),
            expression: self.parts[part].expression(),
            rhythm_output_level: drum
                .map(|d| RhythmTemp(self.memory.rhythm_temp(d)).output_level()),
            master_vol: self.memory.system()[22],
            master_tune_pitch_delta: self.master_tune_pitch_delta,
            pitch_bend: self.parts[part].pitch_bend(),
            modulation: u32::from(self.parts[part].modulation()),
            patch_key_shift: patch.key_shift(),
            patch_fine_tune: patch.fine_tune(),
            nice_amp_ramp: self.nice.amp_ramp,
        }
    }

    /// The system area's reverb bytes applied: the mode selects a chip
    /// (a change powers a fresh one), time and level re-aim the running
    /// chip, and zero time with zero level switches it out entirely.
    fn refresh_reverb(&mut self) {
        let system = self.memory.system();
        let (mode, time, level) = (system[1], system[2], system[3]);
        if time == 0 && level == 0 {
            self.reverb = None;
            return;
        }
        if self.reverb.as_ref().map(Reverb::mode) != Some(mode) {
            self.reverb = Some(Reverb::new(
                mode,
                self.quirks.default_reverb_mt32_compatible,
            ));
        }
        if let Some(reverb) = self.reverb.as_mut() {
            reverb.set_parameters(time, level);
        }
    }

    /// Render interleaved stereo, as the module's jacks give it: at the
    /// analogue model's rate. Until a message has arrived the machinery
    /// is held still, though the analogue filters' clock keeps moving.
    pub fn render(&mut self, stream: &mut [(i16, i16)]) {
        if !self.activated {
            stream.fill((0, 0));
            let native = self.analog.dac_streams_length(stream.len());
            self.rendered_sample_count = self.rendered_sample_count.wrapping_add(native as u32);
            self.analog.skip(stream.len());
            self.display.refresh(self.rendered_sample_count);
            return;
        }
        let mut done = 0;
        while done < stream.len() {
            let this_pass = (stream.len() - done).min(MAX_SAMPLES_PER_RUN);
            let native_len = self.analog.dac_streams_length(this_pass);
            self.render_streams(native_len);
            let Self {
                analog, streams, ..
            } = self;
            analog.process(&mut stream[done..done + this_pass], streams);
            done += this_pass;
        }
    }

    /// The digital half's streams for `len` native frames, with the
    /// reference's own pacing: while a poly aborts, one frame at a time
    /// and the queue waits; a waiting message plays and is followed by
    /// exactly one frame, so zero-duration notes still sound; otherwise
    /// the run goes through whole.
    fn render_streams(&mut self, len: usize) {
        let mut streams = std::mem::take(&mut self.streams);
        streams.clear(len);
        self.streams = streams;
        let mut done = 0;
        while done < len {
            let mut chunk = 1;
            if !self.is_aborting_poly() {
                if self.queue.is_empty() {
                    chunk = len - done;
                } else {
                    self.play_one_queued();
                }
            }
            self.produce_chunk(done, chunk);
            done += chunk;
        }
    }

    fn produce_chunk(&mut self, offset: usize, len: usize) {
        let mut streams = std::mem::take(&mut self.streams);
        let span = offset..offset + len;

        for id in 0..PARTIAL_COUNT {
            if !self.partials[id].is_active()
                || self.partials[id].already_outputed
                || self.partials[id].is_ring_modulating_slave()
                || self.partials[id].poly().is_none()
            {
                continue;
            }
            if self.partials[id].should_reverb() {
                let (l, r) = (
                    &mut streams.dry_l[span.clone()],
                    &mut streams.dry_r[span.clone()],
                );
                self.render_partial_chunk(id, l, r);
            } else {
                let (l, r) = (
                    &mut streams.non_l[span.clone()],
                    &mut streams.non_r[span.clone()],
                );
                self.render_partial_chunk(id, l, r);
            }
        }

        // The NICE DAC transform doubles into the clip; the dry stream
        // takes it before the reverb would, the non-reverb before the
        // analogue stage.
        for sample in streams.dry_l[span.clone()]
            .iter_mut()
            .chain(streams.dry_r[span.clone()].iter_mut())
        {
            *sample = nice_dac(*sample);
        }
        for sample in streams.non_l[span.clone()]
            .iter_mut()
            .chain(streams.non_r[span.clone()].iter_mut())
        {
            *sample = nice_dac(*sample);
        }
        // The wet pair comes off the reverb chip, fed the DAC-shaped dry
        // streams; with no chip selected it stays silent.
        if let Some(reverb) = self.reverb.as_mut() {
            reverb.process(
                &streams.dry_l[span.clone()],
                &streams.dry_r[span.clone()],
                &mut streams.wet_l[span.clone()],
                &mut streams.wet_r[span],
            );
        }
        self.streams = streams;

        for partial in self.partials.iter_mut() {
            partial.already_outputed = false;
        }
        self.rendered_sample_count = self.rendered_sample_count.wrapping_add(len as u32);
        self.display.refresh(self.rendered_sample_count);
    }

    /// One master partial's whole chunk, driving its slave if it has one:
    /// the reference's loop, deaths deferred to the end of the chunk where
    /// their cascades cannot change what the loop reads.
    fn render_partial_chunk(&mut self, master: usize, left: &mut [i16], right: &mut [i16]) {
        self.partials[master].already_outputed = true;
        let part = self.partials[master].owner_part().unwrap_or(0);
        let drum = self.partials[master].rhythm_key();
        let mut master_died = false;
        let mut slave_died = false;

        for n in 0..left.len() {
            let live = self.live_values(part, drum);
            let slave = self.partials[master].pair();
            if !self.partials[master].tva_playing()
                || !self.partials[master].la32.is_active(Pair::Master)
            {
                master_died = true;
                break;
            }
            let master_source = self.partials[master].param_source().unwrap();
            let Self {
                memory,
                partials,
                tables,
                waves,
                quirks,
                jitter,
                pcm,
                ..
            } = self;
            let param = PartialParam(memory.partial_param(master_source));
            let amp = partials[master].next_amp(tables, param, &live, quirks);
            let pitch = partials[master].next_pitch(tables, param, waves, &live, quirks, jitter);
            let cutoff = partials[master].next_cutoff(tables, param);
            partials[master].la32.generate_next_sample(
                tables,
                pcm,
                Pair::Master,
                amp,
                pitch,
                cutoff,
            );
            if let Some(slave_id) = slave {
                if self.partials[master].has_ring_modulating_slave() {
                    let slave_part = self.partials[slave_id].owner_part().unwrap_or(0);
                    let slave_drum = self.partials[slave_id].rhythm_key();
                    let slave_live = self.live_values(slave_part, slave_drum);
                    let slave_source = self.partials[slave_id].param_source().unwrap();
                    let Self {
                        memory,
                        partials,
                        tables,
                        waves,
                        quirks,
                        jitter,
                        pcm,
                        ..
                    } = self;
                    let sparam = PartialParam(memory.partial_param(slave_source));
                    let (m, s) = two(partials, master, slave_id);
                    let s_amp = s.next_amp(tables, sparam, &slave_live, quirks);
                    let s_pitch = s.next_pitch(tables, sparam, waves, &slave_live, quirks, jitter);
                    let s_cutoff = s.next_cutoff(tables, sparam);
                    m.la32
                        .generate_next_sample(tables, pcm, Pair::Slave, s_amp, s_pitch, s_cutoff);
                    if !s.tva_playing() || !m.la32.is_active(Pair::Slave) {
                        m.la32.deactivate(Pair::Slave);
                        slave_died = true;
                        if m.mix_type() == 2 {
                            m.la32.deactivate(Pair::Master);
                            master_died = true;
                        }
                    }
                }
            }
            if master_died {
                break;
            }
            let sample = self.partials[master].la32.next_out_sample(&self.tables);
            self.partials[master].mix_sample(sample, &mut left[n], &mut right[n]);
        }

        if slave_died {
            if let Some(slave_id) = self.partials[master].pair() {
                self.deactivate_partial(slave_id);
            }
        }
        if master_died {
            self.deactivate_partial(master);
        }
    }
}

/// Two distinct partials borrowed at once, by index.
fn two(partials: &mut [Partial], a: usize, b: usize) -> (&mut Partial, &mut Partial) {
    debug_assert_ne!(a, b);
    if a < b {
        let (left, right) = partials.split_at_mut(b);
        (&mut left[a], &mut right[0])
    } else {
        let (left, right) = partials.split_at_mut(a);
        (&mut right[0], &mut left[b])
    }
}

/// The NICE DAC transform: double into the reference's exact clip.
fn nice_dac(sample: i16) -> i16 {
    crate::analog::clip(i32::from(sample) << 1)
}
