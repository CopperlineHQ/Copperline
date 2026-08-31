// SPDX-License-Identifier: GPL-3.0-or-later

//! copperhf.device: a register-level virtual block-storage board, M1 slice
//! (board + register protocol, synchronous I/O; see
//! `COPPERHF-DEVICE-PLAN.md`).
//!
//! Up to [`NUM_UNITS`] units, each an optional [`HardDriveImage`], are
//! addressed by a raw unit number (not a guest `Unit` pointer -- see
//! `guest/copperhf/copperhf_board.h`) through a doorbell/completion-queue
//! protocol modelled on real trackdisk-style hardware but simplified: a
//! request handed to [`CopperhfBoard::write`] via `CHF_DOORBELL` is executed
//! synchronously (no worker thread yet -- that is M5's job) and its pointer
//! is pushed onto a completion queue the guest drains through
//! `CHF_COMPLETE_GET`/`CHF_COMPLETE_ACK`. INT2 is asserted whenever the
//! queue is non-empty and `CHF_IRQ_ENABLE` is set.
//!
//! The register offsets, IOStdReq field offsets, command numbers, and error
//! codes below all mirror `guest/copperhf/copperhf_board.h`; keep the two in
//! sync.

use crate::harddrive::{HardDriveImage, RDB_HEADS, RDB_SPT};
use crate::zorro_device::{DeviceHost, ZorroDevice};
use std::collections::VecDeque;

/// Unit slots. Matches `CHF_UNITS`/`CHF_NUM_UNITS` in the shared header.
pub const NUM_UNITS: usize = 7;

// Register offsets -- see guest/copperhf/copperhf_board.h.
const CHF_MAGIC: u32 = 0x4000;
const CHF_VERSION: u32 = 0x4004;
const CHF_UNITS: u32 = 0x4006;
const CHF_UNIT_PRESENT: u32 = 0x4008;
const CHF_UNIT_RDONLY: u32 = 0x400A;
const CHF_UNIT_SELECT: u32 = 0x400C;
const CHF_CHANGE_COUNT: u32 = 0x400E;
const CHF_UNIT_BLOCKS: u32 = 0x4010;
const CHF_DOORBELL: u32 = 0x4020;
const CHF_COMPLETE_GET: u32 = 0x4028;
const CHF_COMPLETE_ACK: u32 = 0x402C;
const CHF_IRQ_STATUS: u32 = 0x4030;
const CHF_IRQ_ENABLE: u32 = 0x4032;

const CHF_MAGIC_VALUE: u32 = 0x4350_4846; // "CPHF"
const CHF_PROTOCOL_VERSION: u16 = 1;

// IOStdReq field offsets, relative to the request pointer.
const IO_UNIT: u32 = 24;
const IO_COMMAND: u32 = 28;
const IO_FLAGS_ERROR: u32 = 30; // io_Flags (hi byte) / io_Error (lo byte)
const IO_ACTUAL: u32 = 32;
const IO_LENGTH: u32 = 36;
const IO_DATA: u32 = 40;
const IO_OFFSET: u32 = 44;

// Commands.
const CMD_READ: u16 = 2;
const CMD_WRITE: u16 = 3;
const CMD_UPDATE: u16 = 4;
const CMD_CLEAR: u16 = 5;
const TD_MOTOR: u16 = 9;
const TD_FORMAT: u16 = 11;
const TD_GETGEOMETRY: u16 = 22;

// io_Error values.
const IOERR_OPENFAIL: i8 = -1;
const IOERR_NOCMD: i8 = -3;
const IOERR_BADLENGTH: i8 = -4;
const IOERR_BADADDRESS: i8 = -5;

const SECTOR_SIZE: usize = 512;

/// Bound on the completion queue purely as a sanity backstop: in the
/// synchronous M1 protocol exactly one completion is produced per doorbell
/// write, so the queue only grows if the guest stops draining it. Nothing is
/// ever dropped -- a queue past this length just logs a warning, since a
/// hung guest is a guest bug to diagnose, not data to discard.
const COMPLETION_WARN_LEN: usize = 64;

/// The copperhf.device board: register window, unit table, and the
/// synchronous doorbell/completion protocol described in
/// `guest/copperhf/copperhf_board.h`.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct CopperhfBoard {
    units: [Option<HardDriveImage>; NUM_UNITS],
    /// Disk-change counter per unit, bumped on attach/detach (`CHF_CHANGE_COUNT`).
    change_count: [u16; NUM_UNITS],
    /// TD_MOTOR state per unit; tracked but has no effect on I/O in M1.
    motor: [bool; NUM_UNITS],
    /// `CHF_UNIT_SELECT`: which unit `CHF_CHANGE_COUNT`/`CHF_UNIT_BLOCKS`
    /// report on. Read back as written even when it names no real unit.
    select: u16,
    /// High half of a `CHF_DOORBELL` write, latched until the low half
    /// commits (or a 32-bit write commits both halves at once).
    doorbell_hi: Option<u16>,
    /// Completed request pointers awaiting `CHF_COMPLETE_GET`/`_ACK`.
    completions: VecDeque<u32>,
    /// Snapshot taken by the last `CHF_COMPLETE_GET` high-word read, so the
    /// matching low-word read reflects the same instant even if the queue
    /// changes in between (e.g. an ACK lands between the two word reads).
    complete_get_latch: Option<u32>,
    /// Snapshot taken by the last `CHF_UNIT_BLOCKS` high-word read, for the
    /// same torn-read reason (the selected unit could change in between).
    unit_blocks_latch: Option<u32>,
    irq_enable: bool,
    /// Drained by `take_activity` for the HDD LED.
    activity: bool,
}

impl CopperhfBoard {
    pub fn new() -> Self {
        Self {
            units: Default::default(),
            change_count: [0; NUM_UNITS],
            motor: [false; NUM_UNITS],
            select: 0,
            doorbell_hi: None,
            completions: VecDeque::new(),
            complete_get_latch: None,
            unit_blocks_latch: None,
            irq_enable: false,
            activity: false,
        }
    }

    /// Attach a unit's image, bumping its disk-change counter. Replaces
    /// whatever was there.
    pub fn attach_unit(&mut self, unit: usize, image: HardDriveImage) {
        self.units[unit] = Some(image);
        self.change_count[unit] = self.change_count[unit].wrapping_add(1);
        self.motor[unit] = false;
    }

    /// Detach a unit's image, if any, bumping its disk-change counter.
    pub fn detach_unit(&mut self, unit: usize) -> Option<HardDriveImage> {
        let image = self.units[unit].take();
        if image.is_some() {
            self.change_count[unit] = self.change_count[unit].wrapping_add(1);
        }
        image
    }

    fn present_bitmask(&self) -> u16 {
        let mut mask = 0u16;
        for (i, u) in self.units.iter().enumerate() {
            if u.is_some() {
                mask |= 1 << i;
            }
        }
        mask
    }

    /// Always reports every attached unit as writable in M1.
    ///
    /// Deviation from the milestone note: there is no per-image read-only
    /// flag anywhere in the shared `HardDriveImage` layer today (see
    /// COPPERHF-DEVICE-PLAN.md's "Deferred" section) -- not even for a host
    /// disk, which exposes `is_host_disk()` but no writability query. Wiring
    /// `CHF_UNIT_RDONLY` up to something real is therefore a shared-layer
    /// follow-up like the note says PROTSTATUS is, not something this board
    /// can honestly report on its own.
    fn rdonly_bitmask(&self) -> u16 {
        0
    }

    fn selected_unit(&self) -> Option<usize> {
        let sel = usize::from(self.select);
        (sel < NUM_UNITS).then_some(sel)
    }

    fn change_count_for_selected(&self) -> u16 {
        self.selected_unit().map_or(0, |u| self.change_count[u])
    }

    fn blocks_for_selected(&self) -> u32 {
        self.selected_unit()
            .and_then(|u| self.units[u].as_ref())
            .map_or(0, |img| img.total_sectors() as u32)
    }

    fn read_word(&mut self, off: u32) -> u16 {
        match off {
            CHF_MAGIC => (CHF_MAGIC_VALUE >> 16) as u16,
            _ if off == CHF_MAGIC + 2 => CHF_MAGIC_VALUE as u16,
            CHF_VERSION => CHF_PROTOCOL_VERSION,
            CHF_UNITS => NUM_UNITS as u16,
            CHF_UNIT_PRESENT => self.present_bitmask(),
            CHF_UNIT_RDONLY => self.rdonly_bitmask(),
            CHF_UNIT_SELECT => self.select,
            CHF_CHANGE_COUNT => self.change_count_for_selected(),
            CHF_UNIT_BLOCKS => {
                let v = self.blocks_for_selected();
                self.unit_blocks_latch = Some(v);
                (v >> 16) as u16
            }
            _ if off == CHF_UNIT_BLOCKS + 2 => {
                let v = self
                    .unit_blocks_latch
                    .unwrap_or_else(|| self.blocks_for_selected());
                v as u16
            }
            CHF_COMPLETE_GET => {
                let v = self.completions.front().copied().unwrap_or(0);
                self.complete_get_latch = Some(v);
                (v >> 16) as u16
            }
            _ if off == CHF_COMPLETE_GET + 2 => {
                let v = self
                    .complete_get_latch
                    .unwrap_or_else(|| self.completions.front().copied().unwrap_or(0));
                v as u16
            }
            CHF_IRQ_STATUS => u16::from(!self.completions.is_empty()),
            CHF_IRQ_ENABLE => u16::from(self.irq_enable),
            _ => 0xFFFF,
        }
    }

    fn read_long(&mut self, off: u32) -> u32 {
        match off {
            CHF_MAGIC => CHF_MAGIC_VALUE,
            CHF_UNIT_BLOCKS => self.blocks_for_selected(),
            CHF_COMPLETE_GET => self.completions.front().copied().unwrap_or(0),
            _ => 0xFFFF_FFFF,
        }
    }

    fn write_word(&mut self, off: u32, value: u16, host: &mut DeviceHost) {
        match off {
            CHF_UNIT_SELECT => self.select = value,
            CHF_DOORBELL => self.doorbell_hi = Some(value),
            _ if off == CHF_DOORBELL + 2 => {
                let hi = self.doorbell_hi.take().unwrap_or(0);
                let ptr = (u32::from(hi) << 16) | u32::from(value);
                self.execute_request(ptr, host);
            }
            CHF_COMPLETE_ACK => {
                self.completions.pop_front();
            }
            CHF_IRQ_ENABLE => self.irq_enable = value & 1 != 0,
            _ => {}
        }
    }

    fn write_long(&mut self, off: u32, value: u32, host: &mut DeviceHost) {
        if off == CHF_DOORBELL {
            self.doorbell_hi = None;
            self.execute_request(value, host);
        } else {
            self.write_word(off, (value >> 16) as u16, host);
            self.write_word(off + 2, value as u16, host);
        }
    }

    /// Execute the IOStdReq at `ptr`: read its header fields, dispatch on
    /// `io_Command`, write back `io_Error`/`io_Actual`, and push `ptr` onto
    /// the completion queue.
    fn execute_request(&mut self, ptr: u32, host: &mut DeviceHost) {
        let header = RequestHeader::read(ptr, host);
        let Some(header) = header else {
            // Cannot even read the request back to report an error into it
            // -- log and push the pointer anyway so the guest's completion
            // drain isn't left hanging forever on a request it can never
            // get an answer from any other way.
            log::warn!("copperhf: doorbell {ptr:#010X}: could not read request header");
            self.push_completion(ptr);
            return;
        };

        let (error, actual) = self.run_command(&header, host);

        let error_written = write_u32(host, ptr + IO_ACTUAL, actual)
            && host.dma_write_word(
                ptr + IO_FLAGS_ERROR,
                (u16::from(header.flags) << 8) | u16::from(error as u8),
            );
        if !error_written {
            log::warn!("copperhf: doorbell {ptr:#010X}: could not write back io_Error/io_Actual");
        }
        self.push_completion(ptr);
    }

    fn push_completion(&mut self, ptr: u32) {
        self.completions.push_back(ptr);
        if self.completions.len() > COMPLETION_WARN_LEN {
            log::warn!(
                "copperhf: completion queue has grown to {} entries -- is the guest draining it?",
                self.completions.len()
            );
        }
    }

    /// Run one already-decoded command, returning `(io_Error, io_Actual)`.
    fn run_command(&mut self, header: &RequestHeader, host: &mut DeviceHost) -> (i8, u32) {
        match header.command {
            CMD_READ => self.do_read(header, host),
            CMD_WRITE | TD_FORMAT => self.do_write(header, host),
            CMD_UPDATE => {
                if let Some(unit) = self.valid_unit(header.unit) {
                    if let Err(e) = self.units[unit].as_mut().unwrap().flush() {
                        log::warn!("copperhf: unit {unit}: CMD_UPDATE flush failed: {e}");
                    }
                    (0, 0)
                } else {
                    (IOERR_OPENFAIL, 0)
                }
            }
            CMD_CLEAR => (0, 0),
            TD_MOTOR => {
                let Some(unit) = self.valid_unit(header.unit) else {
                    return (IOERR_OPENFAIL, 0);
                };
                let previous = u32::from(self.motor[unit]);
                self.motor[unit] = header.length != 0;
                (0, previous)
            }
            TD_GETGEOMETRY => self.do_get_geometry(header, host),
            _ => (IOERR_NOCMD, 0),
        }
    }

    /// The unit index if `raw` names a present unit, `None` otherwise
    /// (out of range or an empty slot).
    fn valid_unit(&self, raw: u32) -> Option<usize> {
        let unit = raw as usize;
        (unit < NUM_UNITS && self.units[unit].is_some()).then_some(unit)
    }

    fn do_read(&mut self, header: &RequestHeader, host: &mut DeviceHost) -> (i8, u32) {
        let Some(unit) = self.valid_unit(header.unit) else {
            return (IOERR_OPENFAIL, 0);
        };
        let Some((start_lba, sectors)) = self.check_range(unit, header) else {
            return (IOERR_BADLENGTH, 0);
        };
        // One sector at a time -- never a whole-transfer host allocation, so
        // a full-image CMD_READ costs 512 bytes of host buffer, not the
        // image's size, and a bad io_Data pointer fails on the first sector.
        self.activity = true;
        let image = self.units[unit].as_mut().unwrap();
        let mut sector = [0u8; SECTOR_SIZE];
        for i in 0..sectors {
            if let Err(e) = image.read_sector(start_lba + i, &mut sector) {
                log::warn!("copperhf: unit {unit}: read_sector failed: {e}");
                return (IOERR_BADADDRESS, 0);
            }
            if !dma_write_bytes(host, header.data + (i as u32) * SECTOR_SIZE as u32, &sector) {
                return (IOERR_BADADDRESS, 0);
            }
        }
        (0, header.length)
    }

    fn do_write(&mut self, header: &RequestHeader, host: &mut DeviceHost) -> (i8, u32) {
        let Some(unit) = self.valid_unit(header.unit) else {
            return (IOERR_OPENFAIL, 0);
        };
        let Some((start_lba, sectors)) = self.check_range(unit, header) else {
            return (IOERR_BADLENGTH, 0);
        };
        // Sector-at-a-time for the same reasons as `do_read`; a bad io_Data
        // pointer fails before the image is touched at all.
        self.activity = true;
        let image = self.units[unit].as_mut().unwrap();
        for i in 0..sectors {
            let Some(sector) = dma_read_bytes(
                host,
                header.data + (i as u32) * SECTOR_SIZE as u32,
                SECTOR_SIZE,
            ) else {
                return (IOERR_BADADDRESS, 0);
            };
            if let Err(e) = image.write_sector(start_lba + i, &sector) {
                log::warn!("copperhf: unit {unit}: write_sector failed: {e}");
                return (IOERR_BADADDRESS, 0);
            }
        }
        (0, header.length)
    }

    /// Validate a read/write's alignment and range against a unit's size,
    /// returning `(start_lba, sector_count)` on success.
    fn check_range(&self, unit: usize, header: &RequestHeader) -> Option<(u64, u64)> {
        if header.length == 0
            || !(header.length as usize).is_multiple_of(SECTOR_SIZE)
            || !(header.offset as usize).is_multiple_of(SECTOR_SIZE)
        {
            return None;
        }
        let end = u64::from(header.offset).checked_add(u64::from(header.length))?;
        let total_bytes = self.units[unit].as_ref()?.total_sectors() * SECTOR_SIZE as u64;
        if end > total_bytes {
            return None;
        }
        let start_lba = u64::from(header.offset) / SECTOR_SIZE as u64;
        let sectors = u64::from(header.length) / SECTOR_SIZE as u64;
        Some((start_lba, sectors))
    }

    fn do_get_geometry(&mut self, header: &RequestHeader, host: &mut DeviceHost) -> (i8, u32) {
        let Some(unit) = self.valid_unit(header.unit) else {
            return (IOERR_OPENFAIL, 0);
        };
        if header.length < 32 {
            return (IOERR_BADLENGTH, 0);
        }
        let total_sectors = self.units[unit].as_ref().unwrap().total_sectors() as u32;
        let cylinders = total_sectors / (RDB_HEADS * RDB_SPT);
        let mut geom = [0u8; 32];
        geom[0..4].copy_from_slice(&(SECTOR_SIZE as u32).to_be_bytes());
        geom[4..8].copy_from_slice(&total_sectors.to_be_bytes());
        geom[8..12].copy_from_slice(&cylinders.to_be_bytes());
        geom[12..16].copy_from_slice(&(SECTOR_SIZE as u32).to_be_bytes()); // dg_CylSectors
        geom[16..20].copy_from_slice(&RDB_HEADS.to_be_bytes());
        geom[20..24].copy_from_slice(&RDB_SPT.to_be_bytes());
        geom[24..28].copy_from_slice(&1u32.to_be_bytes()); // MEMF_PUBLIC
                                                           // geom[28] DeviceType = 0, geom[29] Flags = 0, geom[30..32] Reserved = 0
        if !dma_write_bytes(host, header.data, &geom) {
            return (IOERR_BADADDRESS, 0);
        }
        (0, 0)
    }
}

impl Default for CopperhfBoard {
    fn default() -> Self {
        Self::new()
    }
}

/// The IOStdReq fields copperhf reads before dispatching a request.
struct RequestHeader {
    unit: u32,
    command: u16,
    flags: u8,
    length: u32,
    data: u32,
    offset: u32,
}

impl RequestHeader {
    fn read(ptr: u32, host: &DeviceHost) -> Option<Self> {
        let unit = read_u32(host, ptr + IO_UNIT)?;
        let command = host.dma_read_word(ptr + IO_COMMAND)?;
        let flags = (host.dma_read_word(ptr + IO_FLAGS_ERROR)? >> 8) as u8;
        let length = read_u32(host, ptr + IO_LENGTH)?;
        let data = read_u32(host, ptr + IO_DATA)?;
        let offset = read_u32(host, ptr + IO_OFFSET)?;
        Some(Self {
            unit,
            command,
            flags,
            length,
            data,
            offset,
        })
    }
}

/// Read a big-endian u32 through two word-granular DMA reads, `None` if
/// either half is unmapped.
///
/// Deviation from the milestone note: the note suggested using
/// [`DeviceHost::dma_read`]/[`DeviceHost::dma_write`] (the byte-granular
/// bulk helpers), but those substitute `0xFF`/silently drop out-of-range
/// bytes rather than reporting failure, which makes the required
/// `IOERR_BADADDRESS` behaviour unreachable through them. The word-granular
/// `dma_read_word`/`dma_write_word` accessors (`Option`/`bool`-returning)
/// are used instead so a bad guest pointer is actually detectable; that also
/// matches the board's 16-bit-bus register model used everywhere else in
/// this file.
fn read_u32(host: &DeviceHost, addr: u32) -> Option<u32> {
    let hi = host.dma_read_word(addr)?;
    let lo = host.dma_read_word(addr + 2)?;
    Some((u32::from(hi) << 16) | u32::from(lo))
}

fn write_u32(host: &mut DeviceHost, addr: u32, value: u32) -> bool {
    host.dma_write_word(addr, (value >> 16) as u16) && host.dma_write_word(addr + 2, value as u16)
}

/// Word-granular checked bulk read (see [`read_u32`]'s doc comment for why
/// this exists instead of the bulk byte helpers). `len` need not be even;
/// the odd trailing byte, if any, is taken from a word read's high byte.
fn dma_read_bytes(host: &DeviceHost, addr: u32, len: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::with_capacity(len);
    let mut a = addr;
    while buf.len() + 2 <= len {
        let w = host.dma_read_word(a)?;
        buf.push((w >> 8) as u8);
        buf.push(w as u8);
        a = a.wrapping_add(2);
    }
    if buf.len() < len {
        let w = host.dma_read_word(a)?;
        buf.push((w >> 8) as u8);
    }
    Some(buf)
}

fn dma_write_bytes(host: &mut DeviceHost, addr: u32, data: &[u8]) -> bool {
    let mut a = addr;
    let mut chunks = data.chunks_exact(2);
    for chunk in &mut chunks {
        let w = (u16::from(chunk[0]) << 8) | u16::from(chunk[1]);
        if !host.dma_write_word(a, w) {
            return false;
        }
        a = a.wrapping_add(2);
    }
    let rem = chunks.remainder();
    if !rem.is_empty() {
        let w = u16::from(rem[0]) << 8;
        if !host.dma_write_word(a, w) {
            return false;
        }
    }
    true
}

impl ZorroDevice for CopperhfBoard {
    fn read(&mut self, off: u32, size: usize, _host: &mut DeviceHost) -> u32 {
        match size {
            4 => self.read_long(off),
            2 => u32::from(self.read_word(off)),
            _ => 0xFFFF_FFFF,
        }
    }

    fn write(&mut self, off: u32, size: usize, value: u32, host: &mut DeviceHost) {
        match size {
            4 => self.write_long(off, value, host),
            2 => self.write_word(off, value as u16, host),
            _ => {}
        }
    }

    fn peek_word(&self, off: u32) -> Option<u16> {
        match off {
            CHF_MAGIC => Some((CHF_MAGIC_VALUE >> 16) as u16),
            _ if off == CHF_MAGIC + 2 => Some(CHF_MAGIC_VALUE as u16),
            CHF_VERSION => Some(CHF_PROTOCOL_VERSION),
            CHF_UNITS => Some(NUM_UNITS as u16),
            CHF_UNIT_PRESENT => Some(self.present_bitmask()),
            CHF_UNIT_RDONLY => Some(self.rdonly_bitmask()),
            CHF_UNIT_SELECT => Some(self.select),
            CHF_CHANGE_COUNT => Some(self.change_count_for_selected()),
            CHF_UNIT_BLOCKS => Some((self.blocks_for_selected() >> 16) as u16),
            _ if off == CHF_UNIT_BLOCKS + 2 => Some(self.blocks_for_selected() as u16),
            CHF_COMPLETE_GET => Some((self.completions.front().copied().unwrap_or(0) >> 16) as u16),
            _ if off == CHF_COMPLETE_GET + 2 => {
                Some(self.completions.front().copied().unwrap_or(0) as u16)
            }
            CHF_IRQ_STATUS => Some(u16::from(!self.completions.is_empty())),
            CHF_IRQ_ENABLE => Some(u16::from(self.irq_enable)),
            _ => None,
        }
    }

    fn tick(&mut self, _cck: u32, _host: &mut DeviceHost) {
        // M1 is fully synchronous: nothing to advance between register
        // accesses. The worker-thread cadence lands in M5.
    }

    fn int2_line(&self) -> bool {
        self.irq_enable && !self.completions.is_empty()
    }

    fn is_idle(&self) -> bool {
        self.completions.is_empty()
    }

    fn take_activity(&mut self) -> bool {
        std::mem::take(&mut self.activity)
    }

    fn reset(&mut self) {
        // Attached media and change counters survive a reset -- only
        // transient protocol/session state (in-flight doorbell latch,
        // pending completions, IRQ enable, motor state) is cleared, matching
        // real trackdisk-style hardware's power-on defaults.
        self.motor = [false; NUM_UNITS];
        self.doorbell_hi = None;
        self.completions.clear();
        self.complete_get_latch = None;
        self.unit_blocks_latch = None;
        self.irq_enable = false;
        self.activity = false;
    }

    fn kind(&self) -> &'static str {
        "copperhf"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Memory;
    use crate::zorro::ZorroChain;
    use std::io::Write;

    fn memory() -> Memory {
        Memory {
            chip_ram: vec![0u8; 0x10_0000],
            slow_ram: Vec::new(),
            mb_ram: Vec::new(),
            accel_ram: Vec::new(),
            rom: Vec::new(),
            overlay: false,
            zorro: ZorroChain::default(),
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs: Vec::new(),
            wcs_write_protected: false,
        }
    }

    /// A tiny flat image the shared harddrive layer will *not* wrap in a
    /// synthesized RDB: `HardDriveImage::open` only does that for a "bare
    /// partition hardfile" (boot block starting `DOS`, no `RDSK` in the
    /// first 16 sectors). Starting the image with neither keeps every LBA a
    /// direct file offset, which is the simplest fixture for exercising the
    /// register protocol.
    fn temp_image(name: &str, sectors: usize) -> std::path::PathBuf {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "copperline-copperhf-{}-{unique}-{name}",
            std::process::id()
        ));
        let mut bytes = vec![0u8; sectors * SECTOR_SIZE];
        // Fill with a recognizable, non-"DOS"/"RDSK" pattern so the shared
        // layer treats this as a flat block device.
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        path
    }

    fn open_image(name: &str, sectors: usize) -> HardDriveImage {
        let path = temp_image(name, sectors);
        HardDriveImage::open(
            &path,
            "CHF0",
            "copperhf",
            None,
            0,
            crate::diskimage::FileSystem::OFS,
        )
        .unwrap()
    }

    /// Write a big-endian IOStdReq into guest chip RAM at `ptr`, returning
    /// nothing -- callers read fields back through the board's own DMA path.
    #[allow(clippy::too_many_arguments)]
    fn write_request(
        mem: &mut Memory,
        ptr: u32,
        unit: u32,
        command: u16,
        flags: u8,
        length: u32,
        data: u32,
        offset: u32,
    ) {
        mem.chip_ram[ptr as usize + IO_UNIT as usize..][..4].copy_from_slice(&unit.to_be_bytes());
        mem.chip_ram[ptr as usize + IO_COMMAND as usize..][..2]
            .copy_from_slice(&command.to_be_bytes());
        mem.chip_ram[ptr as usize + IO_FLAGS_ERROR as usize] = flags;
        mem.chip_ram[ptr as usize + IO_ACTUAL as usize..][..4].fill(0);
        mem.chip_ram[ptr as usize + IO_LENGTH as usize..][..4]
            .copy_from_slice(&length.to_be_bytes());
        mem.chip_ram[ptr as usize + IO_DATA as usize..][..4].copy_from_slice(&data.to_be_bytes());
        mem.chip_ram[ptr as usize + IO_OFFSET as usize..][..4]
            .copy_from_slice(&offset.to_be_bytes());
    }

    fn io_error(mem: &Memory, ptr: u32) -> i8 {
        mem.chip_ram[ptr as usize + IO_FLAGS_ERROR as usize + 1] as i8
    }

    fn io_actual(mem: &Memory, ptr: u32) -> u32 {
        u32::from_be_bytes(
            mem.chip_ram[ptr as usize + IO_ACTUAL as usize..][..4]
                .try_into()
                .unwrap(),
        )
    }

    fn ring_doorbell_long(board: &mut CopperhfBoard, host: &mut DeviceHost, ptr: u32) {
        board.write(CHF_DOORBELL, 4, ptr, host);
    }

    fn ring_doorbell_words(board: &mut CopperhfBoard, host: &mut DeviceHost, ptr: u32) {
        board.write(CHF_DOORBELL, 2, ptr >> 16, host);
        board.write(CHF_DOORBELL + 2, 2, ptr & 0xFFFF, host);
    }

    #[test]
    fn identity_registers_report_magic_version_and_unit_count() {
        let mut board = CopperhfBoard::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(CHF_MAGIC, 4, &mut host), CHF_MAGIC_VALUE);
        assert_eq!(
            board.read(CHF_VERSION, 2, &mut host),
            u32::from(CHF_PROTOCOL_VERSION)
        );
        assert_eq!(board.read(CHF_UNITS, 2, &mut host), NUM_UNITS as u32);
    }

    #[test]
    fn unit_present_bitmask_reflects_attach_and_detach() {
        let mut board = CopperhfBoard::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(CHF_UNIT_PRESENT, 2, &mut host), 0);
        board.attach_unit(2, open_image("present", 16));
        assert_eq!(board.read(CHF_UNIT_PRESENT, 2, &mut host), 0b100);
        board.detach_unit(2);
        assert_eq!(board.read(CHF_UNIT_PRESENT, 2, &mut host), 0);
    }

    #[test]
    fn select_and_blocks_report_the_selected_units_size() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(3, open_image("blocks", 40));
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(CHF_UNIT_SELECT, 2, 3, &mut host);
        assert_eq!(board.read(CHF_UNIT_SELECT, 2, &mut host), 3);
        assert_eq!(board.read(CHF_UNIT_BLOCKS, 4, &mut host), 40);
        // An unselected/absent unit reports zero blocks.
        board.write(CHF_UNIT_SELECT, 2, 5, &mut host);
        assert_eq!(board.read(CHF_UNIT_BLOCKS, 4, &mut host), 0);
        // Out-of-range select values read back as written; queries read 0.
        board.write(CHF_UNIT_SELECT, 2, 99, &mut host);
        assert_eq!(board.read(CHF_UNIT_SELECT, 2, &mut host), 99);
        assert_eq!(board.read(CHF_CHANGE_COUNT, 2, &mut host), 0);
    }

    #[test]
    fn change_count_bumps_on_attach_and_detach() {
        let mut board = CopperhfBoard::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(CHF_UNIT_SELECT, 2, 0, &mut host);
        assert_eq!(board.read(CHF_CHANGE_COUNT, 2, &mut host), 0);
        board.attach_unit(0, open_image("changecount", 16));
        assert_eq!(board.read(CHF_CHANGE_COUNT, 2, &mut host), 1);
        board.detach_unit(0);
        assert_eq!(board.read(CHF_CHANGE_COUNT, 2, &mut host), 2);
    }

    #[test]
    fn cmd_read_round_trips_data_and_reports_success() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("read", 16));
        let mut mem = memory();
        // Seed the underlying image with known bytes by writing through the
        // device itself first would be circular; instead read back what the
        // fixture wrote (the i % 251 pattern from temp_image).
        let ptr = 0x1000u32;
        let data_addr = 0x2000u32;
        write_request(&mut mem, ptr, 0, CMD_READ, 0, 512, data_addr, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);

        assert_eq!(io_error(&mem, ptr), 0);
        assert_eq!(io_actual(&mem, ptr), 512);
        let expected: Vec<u8> = (0..512u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(
            &mem.chip_ram[data_addr as usize..data_addr as usize + 512],
            &expected[..]
        );
    }

    #[test]
    fn cmd_write_persists_and_reads_back() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("write", 16));
        let mut mem = memory();
        let ptr = 0x1000u32;
        let data_addr = 0x2000u32;
        let pattern: Vec<u8> = (0..512u32).map(|i| (i * 3 + 7) as u8).collect();
        mem.chip_ram[data_addr as usize..data_addr as usize + 512].copy_from_slice(&pattern);
        write_request(&mut mem, ptr, 0, CMD_WRITE, 0, 512, data_addr, 512);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), 0);
        assert_eq!(io_actual(&mem, ptr), 512);

        // Read it back through a second request.
        let ptr2 = 0x1100u32;
        let readback_addr = 0x3000u32;
        write_request(&mut mem, ptr2, 0, CMD_READ, 0, 512, readback_addr, 512);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr2);
        assert_eq!(io_error(&mem, ptr2), 0);
        assert_eq!(
            &mem.chip_ram[readback_addr as usize..readback_addr as usize + 512],
            &pattern[..]
        );
    }

    #[test]
    fn doorbell_via_two_word_writes_matches_a_single_long_write() {
        let mut board_a = CopperhfBoard::new();
        board_a.attach_unit(0, open_image("words-a", 16));
        let mut board_b = CopperhfBoard::new();
        board_b.attach_unit(0, open_image("words-b", 16));

        let mut mem_a = memory();
        let mut mem_b = memory();
        let ptr = 0x1000u32;
        let data_addr = 0x2000u32;
        write_request(&mut mem_a, ptr, 0, CMD_READ, 0, 512, data_addr, 0);
        write_request(&mut mem_b, ptr, 0, CMD_READ, 0, 512, data_addr, 0);

        let mut host_a = DeviceHost::new(&mut mem_a);
        ring_doorbell_long(&mut board_a, &mut host_a, ptr);
        let mut host_b = DeviceHost::new(&mut mem_b);
        ring_doorbell_words(&mut board_b, &mut host_b, ptr);

        assert_eq!(io_error(&mem_a, ptr), io_error(&mem_b, ptr));
        assert_eq!(io_actual(&mem_a, ptr), io_actual(&mem_b, ptr));
        assert_eq!(
            board_a.completions.front(),
            board_b.completions.front(),
            "both commit forms should enqueue the same pointer"
        );
    }

    #[test]
    fn high_word_write_alone_does_not_commit() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("nocommit", 16));
        let mut mem = memory();
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, CMD_READ, 0, 512, 0x2000, 0);
        let mut host = DeviceHost::new(&mut mem);
        board.write(CHF_DOORBELL, 2, ptr >> 16, &mut host);
        assert!(board.completions.is_empty());
    }

    #[test]
    fn torn_complete_get_read_reflects_one_consistent_snapshot() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("torn", 16));
        let mut mem = memory();
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, CMD_CLEAR, 0, 0, 0, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(board.completions.len(), 1);

        // Read the high word (latches the snapshot), then ACK (which pops
        // the queue) *before* reading the low word. The low-word read must
        // still reflect the pre-ACK snapshot, not the now-empty queue.
        let hi = board.read(CHF_COMPLETE_GET, 2, &mut host);
        board.write(CHF_COMPLETE_ACK, 2, 0, &mut host);
        assert!(board.completions.is_empty());
        let lo = board.read(CHF_COMPLETE_GET + 2, 2, &mut host);
        let reconstructed = (hi << 16) | lo;
        assert_eq!(reconstructed, ptr);
    }

    #[test]
    fn ack_pops_the_queue_and_drops_the_irq_line() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("irq", 16));
        let mut mem = memory();
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, CMD_CLEAR, 0, 0, 0, 0);
        let mut host = DeviceHost::new(&mut mem);

        board.write(CHF_IRQ_ENABLE, 2, 1, &mut host);
        assert!(!board.int2_line());
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert!(
            board.int2_line(),
            "completion queue non-empty + irq enabled"
        );

        board.write(CHF_COMPLETE_ACK, 2, 0, &mut host);
        assert!(!board.int2_line(), "queue drained -- line should drop");
    }

    #[test]
    fn irq_line_stays_low_without_irq_enable() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("noirq", 16));
        let mut mem = memory();
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, CMD_CLEAR, 0, 0, 0, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert!(!board.int2_line(), "irq_enable defaults to 0 at power-on");
    }

    #[test]
    fn misaligned_length_is_bad_length() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("misaligned", 16));
        let mut mem = memory();
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, CMD_READ, 0, 300, 0x2000, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), IOERR_BADLENGTH);
    }

    #[test]
    fn out_of_range_offset_is_bad_length() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("range", 4)); // 4 sectors = 2048 bytes
        let mut mem = memory();
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, CMD_READ, 0, 512, 0x2000, 2048);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), IOERR_BADLENGTH);
    }

    #[test]
    fn bad_data_pointer_is_bad_address() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("badaddr", 16));
        let mut mem = memory();
        let ptr = 0x1000u32;
        let bogus_data = 0xFFFF_0000u32; // well past chip_ram's length
        write_request(&mut mem, ptr, 0, CMD_READ, 0, 512, bogus_data, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), IOERR_BADADDRESS);
    }

    #[test]
    fn unknown_command_is_nocmd() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("nocmd", 16));
        let mut mem = memory();
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, 0xBEEF, 0, 0, 0, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), IOERR_NOCMD);
    }

    #[test]
    fn absent_unit_is_openfail() {
        let mut board = CopperhfBoard::new();
        let mut mem = memory();
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 4, CMD_READ, 0, 512, 0x2000, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), IOERR_OPENFAIL);

        // Also out of the 0..NUM_UNITS range entirely.
        let ptr2 = 0x1100u32;
        write_request(&mut mem, ptr2, 99, CMD_READ, 0, 512, 0x2000, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr2);
        assert_eq!(io_error(&mem, ptr2), IOERR_OPENFAIL);
    }

    #[test]
    fn td_getgeometry_reports_the_configured_geometry() {
        let mut board = CopperhfBoard::new();
        // 16 heads * 32 spt = 512 sectors/cylinder; use 2 cylinders' worth.
        board.attach_unit(0, open_image("geometry", 1024));
        let mut mem = memory();
        let ptr = 0x1000u32;
        let data_addr = 0x2000u32;
        write_request(&mut mem, ptr, 0, TD_GETGEOMETRY, 0, 32, data_addr, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), 0);
        assert_eq!(io_actual(&mem, ptr), 0);

        let g = |off: usize| -> u32 {
            u32::from_be_bytes(
                mem.chip_ram[data_addr as usize + off..][..4]
                    .try_into()
                    .unwrap(),
            )
        };
        assert_eq!(g(0), 512); // dg_SectorSize
        assert_eq!(g(4), 1024); // dg_TotalSectors
        assert_eq!(g(8), 2); // dg_Cylinders
        assert_eq!(g(12), 512); // dg_CylSectors
        assert_eq!(g(16), 16); // dg_Heads
        assert_eq!(g(20), 32); // dg_TrackSectors
        assert_eq!(g(24), 1); // dg_BufMemType
    }

    #[test]
    fn td_getgeometry_requires_at_least_32_bytes() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("geomshort", 16));
        let mut mem = memory();
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, TD_GETGEOMETRY, 0, 16, 0x2000, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), IOERR_BADLENGTH);
    }

    #[test]
    fn td_motor_reports_previous_state_and_latches_new_one() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("motor", 16));
        let mut mem = memory();
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, TD_MOTOR, 0, 1, 0, 0); // turn on
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_actual(&mem, ptr), 0, "was off before this request");
        assert!(board.motor[0]);

        let ptr2 = 0x1100u32;
        write_request(&mut mem, ptr2, 0, TD_MOTOR, 0, 0, 0, 0); // turn off
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr2);
        assert_eq!(io_actual(&mem, ptr2), 1, "was on before this request");
        assert!(!board.motor[0]);
    }

    #[test]
    fn peek_word_has_no_side_effects_on_doorbell_or_completions() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("peek", 16));
        let mut mem = memory();
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, CMD_CLEAR, 0, 0, 0, 0);
        let mut host = DeviceHost::new(&mut mem);

        // Latch a doorbell high word, then peek repeatedly -- must not
        // commit or disturb the latch.
        board.write(CHF_DOORBELL, 2, 0x1234, &mut host);
        for _ in 0..3 {
            let _ = ZorroDevice::peek_word(&board, CHF_COMPLETE_GET);
        }
        assert!(board.completions.is_empty());
        assert_eq!(board.doorbell_hi, Some(0x1234));

        // Commit for real, then peek the completion queue without popping.
        board.write(CHF_DOORBELL + 2, 2, 0x5678, &mut host);
        assert_eq!(board.completions.len(), 1);
        for _ in 0..3 {
            let _ = ZorroDevice::peek_word(&board, CHF_COMPLETE_GET);
            let _ = ZorroDevice::peek_word(&board, CHF_COMPLETE_GET + 2);
        }
        assert_eq!(board.completions.len(), 1, "peek must not pop the queue");
    }

    #[test]
    fn reset_clears_pending_completions_and_irq_enable() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("reset", 16));
        let mut mem = memory();
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, CMD_CLEAR, 0, 0, 0, 0);
        let mut host = DeviceHost::new(&mut mem);
        board.write(CHF_IRQ_ENABLE, 2, 1, &mut host);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert!(board.int2_line());

        board.reset();
        assert!(board.completions.is_empty());
        assert!(!board.irq_enable);
        assert!(!board.int2_line());
        // Attached media survives a reset.
        assert!(board.units[0].is_some());
    }

    #[test]
    fn kind_identifies_the_board() {
        let board = CopperhfBoard::new();
        assert_eq!(board.kind(), "copperhf");
    }
}
