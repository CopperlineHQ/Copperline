// SPDX-License-Identifier: LGPL-2.1-or-later

//! The module's addressable memory: the regions SysEx reads and writes,
//! and the state a unit powers on with.
//!
//! Addresses come in two spellings. The manual and every SysEx message use
//! the printed three-byte form (`0x100000` for the system area); the
//! firmware flattens the three 7-bit bytes into one number and works in
//! that. [`flat`] and [`printed`] convert; everything below speaks flat.
//!
//! Writes clamp byte by byte against per-region limit tables read out of
//! the control ROM -- a value past a parameter's maximum stores the
//! maximum, and a limit of zero write-protects the byte against everything
//! but initialisation. Power-on is built the way the firmware builds it:
//! the timbre banks loaded through the same clamping writes, the rhythm
//! and system defaults copied or set, and every part given its program.

use crate::layout::Layout;

/// A timbre's common parameters: name, structures, mute, sustain.
pub const TIMBRE_COMMON: usize = 14;
/// One partial's parameters.
pub const TIMBRE_PARTIAL: usize = 58;
/// A whole timbre: common parameters and four partials.
pub const TIMBRE: usize = TIMBRE_COMMON + 4 * TIMBRE_PARTIAL;
/// A timbre as the memory banks store it, padded to a round size.
pub const PADDED_TIMBRE: usize = TIMBRE + 10;
/// One patch.
pub const PATCH: usize = 8;
/// One part's patch temporary area: a patch, output level, pan, padding.
pub const PATCH_TEMP: usize = 16;
/// One rhythm key's setup: timbre, level, pan, reverb.
pub const RHYTHM_TEMP: usize = 4;
/// The system area, master tune through master volume.
pub const SYSTEM: usize = 23;

/// How much of a control ROM the engine addresses: the lower half of a
/// second-generation image, all of a first.
pub const CONTROL_ROM_SIZE: usize = 64 * 1024;

/// The printed three-byte address, flattened as the firmware works.
pub const fn flat(printed: u32) -> u32 {
    ((printed & 0x7F_0000) >> 2) | ((printed & 0x7F00) >> 1) | (printed & 0x7F)
}

/// The flat address, spelt the way the manual prints it.
pub const fn printed(flat: u32) -> u32 {
    ((flat & 0x1FC000) << 2) | ((flat & 0x3F80) << 1) | (flat & 0x7F)
}

/// The addressable regions, in the order the engine searches them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    PatchTemp,
    RhythmTemp,
    TimbreTemp,
    Patches,
    Timbres,
    System,
    /// Writes show on the display; nothing is stored to read back.
    Display,
    /// Writes reset the unit; there is nothing here at all.
    Reset,
}

impl Region {
    const ALL: [Region; 8] = [
        Region::PatchTemp,
        Region::RhythmTemp,
        Region::TimbreTemp,
        Region::Patches,
        Region::Timbres,
        Region::System,
        Region::Display,
        Region::Reset,
    ];

    /// Where the region starts, flat.
    pub const fn start(self) -> u32 {
        match self {
            Region::PatchTemp => flat(0x03_0000),
            Region::RhythmTemp => flat(0x03_0110),
            Region::TimbreTemp => flat(0x04_0000),
            Region::Patches => flat(0x05_0000),
            Region::Timbres => flat(0x08_0000),
            Region::System => flat(0x10_0000),
            Region::Display => flat(0x20_0000),
            Region::Reset => flat(0x7F_0000),
        }
    }

    pub const fn entry_size(self) -> u32 {
        match self {
            Region::PatchTemp => PATCH_TEMP as u32,
            Region::RhythmTemp => RHYTHM_TEMP as u32,
            Region::TimbreTemp => TIMBRE as u32,
            Region::Patches => PATCH as u32,
            Region::Timbres => PADDED_TIMBRE as u32,
            Region::System => SYSTEM as u32,
            // Sized so a 20-byte message to an elder unit's 0x207F7F still
            // lands inside, as the engine notes.
            Region::Display => 0x4013,
            Region::Reset => 0x3FFF,
        }
    }

    pub const fn entries(self) -> u32 {
        match self {
            Region::PatchTemp => 9,
            Region::RhythmTemp => 85,
            Region::TimbreTemp => 8,
            Region::Patches => 128,
            Region::Timbres => 64,
            _ => 1,
        }
    }

    pub const fn end(self) -> u32 {
        self.start() + self.entry_size() * self.entries()
    }

    /// The region holding `addr`, if any.
    pub fn find(addr: u32) -> Option<Region> {
        Region::ALL
            .into_iter()
            .find(|r| addr >= r.start() && addr < r.end())
    }
}

/// What a write touched beyond the bytes themselves, for the layers above
/// to act on: the synth refreshes what a region feeds, the display shows
/// what was sent to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Touched {
    /// A write landed in a backed region: which one, and the byte
    /// window it covered, region-relative -- the refreshes it triggers
    /// are windowed the same way the firmware's are.
    Ram {
        region: Region,
        offset: usize,
        len: usize,
    },
    /// A write into the display window: where in it, and what arrived.
    Display {
        offset: u32,
        data: Vec<u8>,
    },
    Reset,
}

/// The module's RAM, with the limit tables its control ROM imposes.
pub struct Memory {
    patch_temp: [u8; 9 * PATCH_TEMP],
    rhythm_temp: [u8; 85 * RHYTHM_TEMP],
    timbre_temp: [u8; 8 * TIMBRE],
    patches: [u8; 128 * PATCH],
    /// All four banks -- A, B, memory, rhythm -- though only the memory
    /// bank is addressable from outside, as on the unit.
    timbres: Box<[u8; 256 * PADDED_TIMBRE]>,
    system: [u8; SYSTEM],
    max_patch: [u8; 16],
    max_rhythm: [u8; 4],
    max_system: [u8; SYSTEM],
    /// Built from the ROM's common-plus-one-partial table, expanded to a
    /// whole padded timbre for direct indexing.
    max_padded_timbre: [u8; PADDED_TIMBRE],
}

impl Memory {
    /// The state a unit with this control ROM powers on in.
    pub fn power_on(control_image: &[u8], layout: &Layout) -> Result<Memory, String> {
        let control = control_image
            .get(..CONTROL_ROM_SIZE)
            .ok_or_else(|| "control image shorter than the addressed half".to_string())?;

        let mut max_padded_timbre = [0u8; PADDED_TIMBRE];
        let max_at = usize::from(layout.timbre_max_table);
        max_padded_timbre[..TIMBRE_COMMON + TIMBRE_PARTIAL]
            .copy_from_slice(&control[max_at..max_at + TIMBRE_COMMON + TIMBRE_PARTIAL]);
        for p in 1..4 {
            let at = TIMBRE_COMMON + p * TIMBRE_PARTIAL;
            max_padded_timbre.copy_within(TIMBRE_COMMON..TIMBRE_COMMON + TIMBRE_PARTIAL, at);
        }

        let table = |at: u16, len: usize| &control[usize::from(at)..usize::from(at) + len];
        let mut memory = Memory {
            patch_temp: [0; 9 * PATCH_TEMP],
            rhythm_temp: [0; 85 * RHYTHM_TEMP],
            timbre_temp: [0; 8 * TIMBRE],
            patches: [0; 128 * PATCH],
            timbres: vec![0u8; 256 * PADDED_TIMBRE]
                .into_boxed_slice()
                .try_into()
                .expect("the size is the size"),
            system: [0; SYSTEM],
            max_patch: table(layout.patch_max_table, 16).try_into().unwrap(),
            max_rhythm: table(layout.rhythm_max_table, 4).try_into().unwrap(),
            max_system: table(layout.system_max_table, SYSTEM).try_into().unwrap(),
            max_padded_timbre,
        };

        // The timbre banks, loaded through the same clamped writes the
        // firmware uses: A, B, then the rhythm bank, which is stored
        // compressed in every ROM.
        memory.init_timbres(
            control,
            layout.timbre_a_map,
            layout.timbre_a_offset,
            64,
            0,
            layout.timbre_a_compressed,
        )?;
        memory.init_timbres(
            control,
            layout.timbre_b_map,
            layout.timbre_b_offset,
            64,
            64,
            layout.timbre_b_compressed,
        )?;
        memory.init_timbres(
            control,
            layout.timbre_r_map,
            0,
            usize::from(layout.timbre_r_count),
            192,
            true,
        )?;
        if layout.timbre_r_count == 30 {
            // The elder units wrap the thirty rhythm timbres over the next
            // thirty slots; the last four misbehave on hardware and are
            // zeroed rather than modelled.
            self_copy(&mut memory.timbres[..], 192, 222, 30);
            memory.timbres[252 * PADDED_TIMBRE..].fill(0);
        }
        // The memory bank powers on cleared, as the CM-64 initialises it.
        memory.timbres[128 * PADDED_TIMBRE..192 * PADDED_TIMBRE].fill(0);

        // The rhythm setup and partial reserve come straight from the ROM,
        // unclamped; the rest of the system area is the firmware's
        // documented power-on state.
        let rhythm_len = usize::from(layout.rhythm_settings_count) * RHYTHM_TEMP;
        memory.rhythm_temp[..rhythm_len].copy_from_slice(table(layout.rhythm_settings, rhythm_len));

        for (i, patch) in memory.patches.chunks_exact_mut(PATCH).enumerate() {
            patch.copy_from_slice(&default_patch((i / 64) as u8, (i % 64) as u8));
        }

        memory.system[0] = 0x4A; // master tune: the manual's 442 Hz
        memory.system[1] = 0; // reverb mode: room
        memory.system[2] = 5; // reverb time
        memory.system[3] = 3; // reverb level
        memory.system[4..13].copy_from_slice(table(layout.reserve_settings, 9));
        for i in 0..9u8 {
            memory.system[13 + usize::from(i)] = i + 1; // parts on channels 2-10
        }
        memory.system[22] = 100; // master volume

        for part in 0..9 {
            let at = part * PATCH_TEMP;
            memory.patch_temp[at..at + PATCH].copy_from_slice(&default_patch(0, 0));
            memory.patch_temp[at + 8] = 80; // output level
            memory.patch_temp[at + 9] = control[usize::from(layout.pan_settings) + part];
            memory.patch_temp[at + 10..at + PATCH_TEMP].fill(0);
            memory.patch_temp[at + 11] = 127;
        }
        // Parts 1-8 take their programs; the rhythm part has none.
        for part in 0..8 {
            let program = usize::from(control[usize::from(layout.program_settings) + part]);
            let patch: [u8; PATCH] = memory.patches[program * PATCH..(program + 1) * PATCH]
                .try_into()
                .unwrap();
            memory.patch_temp[part * PATCH_TEMP..part * PATCH_TEMP + PATCH].copy_from_slice(&patch);
            let timbre = usize::from(patch[0]) * 64 + usize::from(patch[1]);
            let from = timbre * PADDED_TIMBRE;
            memory.timbre_temp[part * TIMBRE..(part + 1) * TIMBRE]
                .copy_from_slice(&memory.timbres[from..from + TIMBRE]);
        }
        Ok(memory)
    }

    /// One timbre bank out of the ROM, through clamped writes as the
    /// firmware loads it.
    fn init_timbres(
        &mut self,
        control: &[u8],
        map: u16,
        offset: u16,
        count: usize,
        start: usize,
        compressed: bool,
    ) -> Result<(), String> {
        for n in 0..count {
            let at = usize::from(map) + 2 * n;
            let address = usize::from(u16::from_le_bytes([control[at], control[at + 1]]))
                + usize::from(offset);
            if compressed {
                self.init_compressed_timbre(start + n, control.get(address..).unwrap_or(&[]))
                    .map_err(|e| format!("timbre {} of bank at 0x{map:04X}: {e}", start + n))?;
            } else {
                if address + TIMBRE > CONTROL_ROM_SIZE {
                    return Err(format!(
                        "timbre {} of bank at 0x{map:04X} points outside the ROM",
                        start + n
                    ));
                }
                self.write_timbre_init(start + n, 0, &control[address..address + TIMBRE]);
            }
        }
        Ok(())
    }

    /// A compressed timbre: muted partials are not stored (except a muted
    /// first), so each takes the previous unmuted partial's bytes. The
    /// mute mask is read back from what the clamped write stored, exactly
    /// as the engine reads it.
    fn init_compressed_timbre(&mut self, timbre: usize, src: &[u8]) -> Result<(), String> {
        if src.len() < TIMBRE_COMMON {
            return Err("no room for the common parameters".to_string());
        }
        self.write_timbre_init(timbre, 0, &src[..TIMBRE_COMMON]);
        let mute = self.timbres[timbre * PADDED_TIMBRE + 12];
        let mut src_pos = TIMBRE_COMMON;
        let mut mem_pos = TIMBRE_COMMON;
        for partial in 0..4 {
            if partial != 0 && (mute >> partial) & 1 == 0 {
                src_pos -= TIMBRE_PARTIAL;
            } else if src_pos + TIMBRE_PARTIAL >= src.len() {
                return Err("a partial runs off the ROM".to_string());
            }
            self.write_timbre_init(timbre, mem_pos, &src[src_pos..src_pos + TIMBRE_PARTIAL]);
            src_pos += TIMBRE_PARTIAL;
            mem_pos += TIMBRE_PARTIAL;
        }
        Ok(())
    }

    /// An initialisation write into the timbre banks: clamped by the limit
    /// table, with zero limits meaning zero rather than write-protected.
    fn write_timbre_init(&mut self, timbre: usize, off: usize, data: &[u8]) {
        for (i, &byte) in data.iter().enumerate() {
            let limit = self.max_padded_timbre[(off + i) % PADDED_TIMBRE];
            self.timbres[timbre * PADDED_TIMBRE + off + i] = byte.min(limit);
        }
    }

    /// The bytes and limit table a region reads and writes, `None` for the
    /// regions with nothing behind them.
    fn backing(&mut self, region: Region) -> Option<(&mut [u8], Option<&[u8]>)> {
        match region {
            Region::PatchTemp => Some((&mut self.patch_temp, Some(&self.max_patch))),
            Region::RhythmTemp => Some((&mut self.rhythm_temp, Some(&self.max_rhythm))),
            Region::TimbreTemp => Some((&mut self.timbre_temp, Some(&self.max_padded_timbre))),
            Region::Patches => Some((&mut self.patches, Some(&self.max_patch))),
            Region::Timbres => Some((
                &mut self.timbres[128 * PADDED_TIMBRE..192 * PADDED_TIMBRE],
                Some(&self.max_padded_timbre),
            )),
            Region::System => Some((&mut self.system, Some(&self.max_system))),
            Region::Display | Region::Reset => None,
        }
    }

    /// One part's sixteen patch temp bytes.
    pub fn patch_temp(&self, part: usize) -> &[u8] {
        &self.patch_temp[part * PATCH_TEMP..(part + 1) * PATCH_TEMP]
    }

    pub fn patch_temp_mut(&mut self, part: usize) -> &mut [u8] {
        &mut self.patch_temp[part * PATCH_TEMP..(part + 1) * PATCH_TEMP]
    }

    /// One part's timbre temp: the whole timbre it is playing.
    pub fn timbre_temp(&self, part: usize) -> &[u8] {
        &self.timbre_temp[part * TIMBRE..(part + 1) * TIMBRE]
    }

    pub fn timbre_temp_mut(&mut self, part: usize) -> &mut [u8] {
        &mut self.timbre_temp[part * TIMBRE..(part + 1) * TIMBRE]
    }

    /// One timbre out of the four banks, by absolute number.
    pub fn bank_timbre(&self, timbre: usize) -> &[u8] {
        &self.timbres[timbre * PADDED_TIMBRE..timbre * PADDED_TIMBRE + TIMBRE]
    }

    /// One patch's eight bytes.
    pub fn patch(&self, n: usize) -> &[u8] {
        &self.patches[n * PATCH..(n + 1) * PATCH]
    }

    /// One rhythm key's four setup bytes.
    pub fn rhythm_temp(&self, key: usize) -> &[u8] {
        &self.rhythm_temp[key * RHYTHM_TEMP..(key + 1) * RHYTHM_TEMP]
    }

    /// The system area, master tune through master volume.
    pub fn system(&self) -> &[u8] {
        &self.system
    }

    /// The live bytes a partial's parameters resolve to right now, which
    /// is how a SysEx write reaches a note already sounding.
    pub fn partial_param(&self, source: crate::note::ParamSource) -> &[u8] {
        match source {
            crate::note::ParamSource::TimbreTemp { part, partial } => {
                let timbre = self.timbre_temp(part);
                &timbre[TIMBRE_COMMON + partial * TIMBRE_PARTIAL..][..TIMBRE_PARTIAL]
            }
            crate::note::ParamSource::Bank { timbre, partial } => {
                let timbre = self.bank_timbre(timbre);
                &timbre[TIMBRE_COMMON + partial * TIMBRE_PARTIAL..][..TIMBRE_PARTIAL]
            }
        }
    }

    /// A read at a flat address, as an RQ1 answers: one region, clamped to
    /// its end. How many bytes were written into `out`.
    pub fn read(&mut self, addr: u32, out: &mut [u8]) -> usize {
        let Some(region) = Region::find(addr) else {
            return 0;
        };
        let off = (addr - region.start()) as usize;
        let size = (region.entry_size() * region.entries()) as usize;
        let len = out.len().min(size - off);
        match self.backing(region) {
            Some((bytes, _)) => {
                out[..len].copy_from_slice(&bytes[off..off + len]);
                len
            }
            None => 0,
        }
    }

    /// A patch temp write also selects: every touched part reloads its
    /// timbre temp from the banks -- confirmed on hardware -- except the
    /// first touched entry when the write began past the timbre-choice
    /// bytes, and the rhythm part, which has no timbre temp.
    fn reload_touched_timbres(&mut self, off: usize, len: usize) {
        // The entry arithmetic truncates toward zero as the engine's does,
        // which is what makes a zero-length write at an entry's start
        // still count that entry as touched.
        let first = (off / PATCH_TEMP) as isize;
        let last = (off as isize + len as isize - 1) / PATCH_TEMP as isize;
        for part in first..=last.min(8) {
            let part = part as usize;
            if part == 8 || (part as isize == first && off % PATCH_TEMP > 2) {
                continue;
            }
            let at = part * PATCH_TEMP;
            let timbre =
                usize::from(self.patch_temp[at]) * 64 + usize::from(self.patch_temp[at + 1]);
            let from = timbre * PADDED_TIMBRE;
            self.timbre_temp[part * TIMBRE..(part + 1) * TIMBRE]
                .copy_from_slice(&self.timbres[from..from + TIMBRE]);
        }
    }

    /// A device-global write at a flat address, spanning regions as the
    /// firmware lets it. What it touched, in order.
    pub fn write(&mut self, mut addr: u32, mut data: &[u8]) -> Vec<Touched> {
        let mut touched = Vec::new();
        // A message carrying only an address still reaches its region --
        // that is how Display Reset and the empty custom message work --
        // so one round runs even with nothing to store.
        while let Some(region) = Region::find(addr) {
            let take = data.len().min((region.end() - addr) as usize);
            match self.backing(region) {
                Some((bytes, limits)) => {
                    let limits = limits.expect("backed regions carry limits");
                    let entry_size = region.entry_size() as usize;
                    let off = (addr - region.start()) as usize;
                    for (i, &byte) in data[..take].iter().enumerate() {
                        let limit = limits[(off + i) % entry_size];
                        // A zero limit is write-protection, not a zero
                        // maximum: initialisation is the one caller that
                        // means it literally, and it writes elsewhere.
                        if limit != 0 {
                            bytes[off + i] = byte.min(limit);
                        }
                    }
                    if region == Region::PatchTemp {
                        self.reload_touched_timbres(off, take);
                    }
                    touched.push(Touched::Ram {
                        region,
                        offset: off,
                        len: take,
                    });
                }
                None if region == Region::Display => {
                    touched.push(Touched::Display {
                        offset: addr - region.start(),
                        data: data[..take].to_vec(),
                    });
                }
                None => touched.push(Touched::Reset),
            }
            addr += take as u32;
            data = &data[take..];
            if data.is_empty() {
                break;
            }
        }
        touched
    }
}

/// A patch as power-on writes it: a timbre choice and the documented
/// defaults around it.
fn default_patch(group: u8, num: u8) -> [u8; PATCH] {
    [group, num, 24, 50, 12, 0, 1, 0]
}

/// Copy `count` padded timbres from `from` to `to` within the banks.
fn self_copy(timbres: &mut [u8], from: usize, to: usize, count: usize) {
    timbres.copy_within(
        from * PADDED_TIMBRE..(from + count) * PADDED_TIMBRE,
        to * PADDED_TIMBRE,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two spellings convert both ways, and the corners the engine
    /// leans on hold: the system area's flat form, and patch temp running
    /// flush into rhythm temp.
    #[test]
    fn the_address_spellings_agree() {
        for &p in &[0x03_0000, 0x03_0110, 0x04_0000, 0x10_0000, 0x7F_0000] {
            assert_eq!(printed(flat(p)), p, "0x{p:06X}");
        }
        assert_eq!(flat(0x10_0000), 0x40000);
        assert_eq!(
            Region::PatchTemp.end(),
            Region::RhythmTemp.start(),
            "patch temp runs flush into rhythm temp"
        );
    }

    /// Every address belongs to at most one region, and each region's
    /// bounds answer for their own corners.
    #[test]
    fn the_regions_do_not_overlap() {
        for r in Region::ALL {
            assert_eq!(Region::find(r.start()), Some(r));
            assert_eq!(Region::find(r.end() - 1), Some(r));
            for other in Region::ALL {
                if r != other {
                    assert!(
                        r.end() <= other.start() || other.end() <= r.start(),
                        "{r:?} overlaps {other:?}"
                    );
                }
            }
        }
        assert_eq!(Region::find(flat(0x7F_0000) - 1), None);
    }
}
