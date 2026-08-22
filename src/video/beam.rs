// SPDX-License-Identifier: GPL-3.0-or-later

//! Beam-positioned video event indexing shared by render and collision paths.
//!
//! The emulator records CPU/Copper writes in beam order. This module turns
//! those frame-wide logs into visible-line buckets so later video code can
//! replay only the events that can affect the current scanline/pixel.

use super::FB_HEIGHT;
use crate::bus::{BeamChipRamWrite, BeamRegisterWrite};

pub const VISIBLE_START_VPOS: u32 = 0x2C;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BeamLineEvents {
    register_writes: Vec<BeamRegisterWrite>,
    chip_ram_writes: Vec<BeamChipRamWrite>,
    bitplane_pointer_writes: Vec<BeamRegisterWrite>,
    bitplane_data_writes: Vec<BeamRegisterWrite>,
    bitplane_control_writes: Vec<BeamRegisterWrite>,
    video_control_writes: Vec<BeamRegisterWrite>,
    ddf_writes: Vec<BeamRegisterWrite>,
    diw_writes: Vec<BeamRegisterWrite>,
    dmacon_writes: Vec<BeamRegisterWrite>,
    clxcon_writes: Vec<BeamRegisterWrite>,
    palette_writes: Vec<BeamRegisterWrite>,
    sprite_pointer_writes: Vec<BeamRegisterWrite>,
    sprite_register_writes: Vec<BeamRegisterWrite>,
}

// The shared index exposes every event category even though individual render
// and collision consumers only need subsets of it.
#[allow(dead_code)]
impl BeamLineEvents {
    pub fn register_writes(&self) -> &[BeamRegisterWrite] {
        &self.register_writes
    }

    pub fn chip_ram_writes(&self) -> &[BeamChipRamWrite] {
        &self.chip_ram_writes
    }

    pub fn bitplane_pointer_writes(&self) -> &[BeamRegisterWrite] {
        &self.bitplane_pointer_writes
    }

    pub fn bitplane_data_writes(&self) -> &[BeamRegisterWrite] {
        &self.bitplane_data_writes
    }

    pub fn bitplane_control_writes(&self) -> &[BeamRegisterWrite] {
        &self.bitplane_control_writes
    }

    pub fn ddf_writes(&self) -> &[BeamRegisterWrite] {
        &self.ddf_writes
    }

    pub fn diw_writes(&self) -> &[BeamRegisterWrite] {
        &self.diw_writes
    }

    pub fn dmacon_writes(&self) -> &[BeamRegisterWrite] {
        &self.dmacon_writes
    }

    pub fn clxcon_writes(&self) -> &[BeamRegisterWrite] {
        &self.clxcon_writes
    }

    pub fn palette_writes(&self) -> &[BeamRegisterWrite] {
        &self.palette_writes
    }

    pub fn sprite_pointer_writes(&self) -> &[BeamRegisterWrite] {
        &self.sprite_pointer_writes
    }

    pub fn sprite_register_writes(&self) -> &[BeamRegisterWrite] {
        &self.sprite_register_writes
    }

    pub fn video_control_writes(&self) -> &[BeamRegisterWrite] {
        &self.video_control_writes
    }

    fn push_register(&mut self, event: BeamRegisterWrite) {
        self.register_writes.push(event);
        match event.offset & 0x01FE {
            0x0E0..=0x0F7 => self.bitplane_pointer_writes.push(event),
            0x110..=0x11A => {
                self.bitplane_data_writes.push(event);
                self.video_control_writes.push(event);
            }
            0x100 | 0x102 | 0x104 | 0x106 | 0x108 | 0x10A => {
                self.bitplane_control_writes.push(event);
                self.video_control_writes.push(event);
            }
            0x092 | 0x094 => {
                self.ddf_writes.push(event);
                self.video_control_writes.push(event);
            }
            0x08E | 0x090 | 0x1E4 => {
                self.diw_writes.push(event);
                self.video_control_writes.push(event);
            }
            0x096 => {
                self.dmacon_writes.push(event);
                self.video_control_writes.push(event);
            }
            0x098 | 0x10E => {
                self.clxcon_writes.push(event);
                self.video_control_writes.push(event);
            }
            0x180..=0x1BE => self.palette_writes.push(event),
            0x120..=0x13F => self.sprite_pointer_writes.push(event),
            0x140..=0x17F => self.sprite_register_writes.push(event),
            _ => {}
        }
    }

    fn push_chip_ram(&mut self, write: BeamChipRamWrite) {
        self.chip_ram_writes.push(write);
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BeamEventIndex {
    before_visible: BeamLineEvents,
    lines: Vec<BeamLineEvents>,
    after_visible: BeamLineEvents,
}

#[allow(dead_code)]
impl BeamEventIndex {
    pub fn new(
        register_writes: &[BeamRegisterWrite],
        chip_ram_writes: &[BeamChipRamWrite],
    ) -> Self {
        let mut index = Self {
            before_visible: BeamLineEvents::default(),
            lines: vec![BeamLineEvents::default(); FB_HEIGHT],
            after_visible: BeamLineEvents::default(),
        };

        for &event in register_writes {
            index.bucket_for_vpos_mut(event.vpos).push_register(event);
        }
        for write in chip_ram_writes {
            index
                .bucket_for_vpos_mut(write.vpos)
                .push_chip_ram(write.clone());
        }
        index
    }

    pub fn from_register_writes(register_writes: &[BeamRegisterWrite]) -> Self {
        Self::new(register_writes, &[])
    }

    pub fn before_visible(&self) -> &BeamLineEvents {
        &self.before_visible
    }

    pub fn after_visible(&self) -> &BeamLineEvents {
        &self.after_visible
    }

    pub fn line(&self, line: usize) -> Option<&BeamLineEvents> {
        self.lines.get(line)
    }

    pub fn line_for_vpos(&self, vpos: u32) -> Option<&BeamLineEvents> {
        visible_line_index(vpos).and_then(|line| self.line(line))
    }

    pub fn line_for_beam_y(&self, beam_y: i32) -> Option<&BeamLineEvents> {
        if beam_y < 0 {
            return None;
        }
        self.line_for_vpos(beam_y as u32)
    }

    pub fn register_writes_before_visible_line(
        &self,
        line: usize,
    ) -> impl Iterator<Item = &BeamRegisterWrite> {
        self.before_visible.register_writes.iter().chain(
            self.lines[..line.min(self.lines.len())]
                .iter()
                .flat_map(|line| line.register_writes.iter()),
        )
    }

    pub fn video_control_writes_before_visible_line(
        &self,
        line: usize,
    ) -> impl Iterator<Item = &BeamRegisterWrite> {
        self.before_visible.video_control_writes().iter().chain(
            self.lines[..line.min(self.lines.len())]
                .iter()
                .flat_map(|line| line.video_control_writes().iter()),
        )
    }

    pub fn bitplane_data_writes_before_visible_line(
        &self,
        line: usize,
    ) -> impl Iterator<Item = &BeamRegisterWrite> {
        self.before_visible.bitplane_data_writes().iter().chain(
            self.lines[..line.min(self.lines.len())]
                .iter()
                .flat_map(|line| line.bitplane_data_writes().iter()),
        )
    }

    pub fn sprite_register_writes_before_visible_line(
        &self,
        line: usize,
    ) -> impl Iterator<Item = &BeamRegisterWrite> {
        self.before_visible.sprite_register_writes().iter().chain(
            self.lines[..line.min(self.lines.len())]
                .iter()
                .flat_map(|line| line.sprite_register_writes().iter()),
        )
    }

    fn bucket_for_vpos_mut(&mut self, vpos: u32) -> &mut BeamLineEvents {
        match visible_line_index(vpos) {
            Some(line) => &mut self.lines[line],
            None if vpos < VISIBLE_START_VPOS => &mut self.before_visible,
            None => &mut self.after_visible,
        }
    }
}

pub fn visible_line_index(vpos: u32) -> Option<usize> {
    vpos.checked_sub(VISIBLE_START_VPOS)
        .map(|line| line as usize)
        .filter(|&line| line < FB_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{BeamChipRamWrite, BeamWriteSource};

    fn reg(vpos: u32, hpos: u32, offset: u16) -> BeamRegisterWrite {
        BeamRegisterWrite {
            vpos,
            hpos,
            offset,
            value: offset,
            source: BeamWriteSource::Copper,
        }
    }

    #[test]
    fn indexes_video_writes_by_visible_line_and_register_class() {
        let writes = [
            reg(0x20, 0x10, 0x0100),
            reg(0x2C, 0x20, 0x00E0),
            reg(0x2C, 0x21, 0x0110),
            reg(0x2D, 0x22, 0x0092),
            reg(0x2D, 0x23, 0x008E),
            reg(0x2D, 0x24, 0x0096),
            reg(0x2D, 0x25, 0x0098),
            reg(0x2D, 0x25, 0x010E),
            reg(0x2D, 0x26, 0x0180),
            reg(0x2E, 0x27, 0x0120),
            reg(0x2E, 0x28, 0x0140),
        ];
        let chip_write = BeamChipRamWrite::from_bytes(0x2D, 0x29, 0x40, &[0x12, 0x34]);
        let index = BeamEventIndex::new(&writes, &[chip_write]);

        assert_eq!(index.before_visible().bitplane_control_writes().len(), 1);
        let line0 = index.line(0).unwrap();
        assert_eq!(line0.bitplane_pointer_writes().len(), 1);
        assert_eq!(line0.bitplane_data_writes().len(), 1);

        let line1 = index.line(1).unwrap();
        assert_eq!(line1.ddf_writes().len(), 1);
        assert_eq!(line1.diw_writes().len(), 1);
        assert_eq!(line1.dmacon_writes().len(), 1);
        assert_eq!(line1.clxcon_writes().len(), 2);
        assert_eq!(line1.palette_writes().len(), 1);
        assert_eq!(line1.chip_ram_writes().len(), 1);

        let line2 = index.line(2).unwrap();
        assert_eq!(line2.sprite_pointer_writes().len(), 1);
        assert_eq!(line2.sprite_register_writes().len(), 1);
    }

    #[test]
    fn exposes_prefix_events_without_later_scanlines() {
        let writes = [
            reg(0x2C, 0x10, 0x0100),
            reg(0x2D, 0x10, 0x0102),
            reg(0x2E, 0x10, 0x0104),
        ];
        let index = BeamEventIndex::from_register_writes(&writes);
        let offsets: Vec<_> = index
            .video_control_writes_before_visible_line(2)
            .map(|event| event.offset)
            .collect();

        assert_eq!(offsets, vec![0x0100, 0x0102]);
    }
}
