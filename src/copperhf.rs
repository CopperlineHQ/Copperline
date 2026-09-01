// SPDX-License-Identifier: GPL-3.0-or-later

//! copperhf.device: a register-level virtual block-storage board (board +
//! register protocol, TD64/NSD/`HD_SCSICMD` command coverage, disk-change
//! handling; still synchronous I/O -- async is M5. See
//! `COPPERHF-DEVICE-PLAN.md`).
//!
//! Up to [`NUM_UNITS`] units, each an optional backing image, are addressed
//! by a raw unit number (not a guest `Unit` pointer -- see
//! `guest/copperhf/copperhf_board.h`) through a doorbell/completion-queue
//! protocol modelled on real trackdisk-style hardware but simplified: a
//! request handed to [`CopperhfBoard::write`] via `CHF_DOORBELL` is executed
//! synchronously (no worker thread yet -- that is M5's job) and its pointer
//! is pushed onto a completion queue the guest drains through
//! `CHF_COMPLETE_GET`/`CHF_COMPLETE_ACK`. INT2 is asserted whenever
//! `CHF_IRQ_ENABLE` is set and either the completion queue is non-empty or
//! a unit's media has changed and not yet been acked (`CHF_CHANGED_MASK`).
//! "Present" (a slot is configured) and "media" (the slot currently has
//! something in it) are tracked separately, so `TD_EJECT` and a runtime
//! CCP `copperhf.eject` behave like pulling a disk out of a still-attached
//! drive rather than unplugging the drive itself.
//!
//! The register offsets, IOStdReq field offsets, command numbers, and error
//! codes below all mirror `guest/copperhf/copperhf_board.h`; keep the two in
//! sync.

use crate::harddrive::{HardDriveImage, RDB_HEADS, RDB_SPT};
use crate::scsi::{ScsiDisk, ScsiExec, CHECK_CONDITION};
use crate::zorro_device::{DeviceHost, ZorroDevice};
use std::collections::VecDeque;

/// Unit slots. Matches `CHF_UNITS`/`CHF_NUM_UNITS` in the shared header.
pub const NUM_UNITS: usize = 7;

/// The guest-side boot ROM (`guest/copperhf/README.md`): a DiagArea plus an
/// exec device stub (Open/Close/Expunge/BeginIO/AbortIO + an INT2
/// completion server), built from `guest/copperhf/{entry.s,device.c,
/// int_handler.s}` and installed by `make` in that directory. A plain
/// `cargo build` just embeds this committed artifact; rebuilding needs the
/// dockerized cross-GCC (`guest/toolchain.mk`).
pub const COPPERHF_ROM: &[u8] = include_bytes!("../assets/copperhf/copperhf_rom.bin");

/// ROM window offset: matches `guest/copperhf/entry.s`'s own `+8` bias
/// (`_diag_entry-_entry_table+8`, hard-coded there against this exact
/// constant) and the filesys/hostsocket convention of leaving the window's
/// first 8 bytes unused ahead of the entry table, even though copperhf has
/// no seglist-header need of its own (see `crate::filesys::ROM_OFFSET`).
pub const ROM_OFFSET: usize = 0x0008;
/// The DiagArea (`BoardSpec::copperhf` points `diag_vec` here): embedded in
/// the ROM at file offset 0x40 (`entry.s`'s `_diag_area`, `.org 0x40`), so
/// window-relative it sits at `ROM_OFFSET + 0x40` -- same derivation as
/// `crate::filesys::DIAG_OFFSET` / `crate::hostsocket::DIAG_OFFSET`. A unit
/// test below locks the byte at this offset to `entry.s`'s own
/// `da_Config` value, so the two files cannot silently drift apart.
pub const DIAG_OFFSET: u16 = ROM_OFFSET as u16 + 0x40;
/// End of the ROM/DiagArea window: the register block starts at
/// `CHF_MAGIC` (0x4000) and the ROM is served read-only strictly below it
/// (`guest/copperhf/Makefile`'s own build-time budget assertion).
const ROM_WINDOW_END: u32 = CHF_MAGIC;

// Register offsets -- see guest/copperhf/copperhf_board.h.
const CHF_MAGIC: u32 = 0x4000;
const CHF_VERSION: u32 = 0x4004;
const CHF_UNITS: u32 = 0x4006;
const CHF_UNIT_PRESENT: u32 = 0x4008;
const CHF_UNIT_RDONLY: u32 = 0x400A;
const CHF_UNIT_SELECT: u32 = 0x400C;
const CHF_CHANGE_COUNT: u32 = 0x400E;
const CHF_UNIT_BLOCKS: u32 = 0x4010;
const CHF_CHANGED_MASK: u32 = 0x4014;
const CHF_CHANGED_ACK: u32 = 0x4016;
const CHF_UNIT_MEDIA: u32 = 0x4018;
const CHF_DOORBELL: u32 = 0x4020;
const CHF_COMPLETE_GET: u32 = 0x4028;
const CHF_COMPLETE_ACK: u32 = 0x402C;
const CHF_IRQ_STATUS: u32 = 0x4030;
const CHF_IRQ_ENABLE: u32 = 0x4032;

const CHF_MAGIC_VALUE: u32 = 0x4350_4846; // "CPHF"
                                          // Version 2 = M4: CHF_CHANGED_MASK/ACK, CHF_UNIT_MEDIA, IRQ_STATUS bit 1,
                                          // TD64/NSD/HD_SCSICMD command coverage. See guest/copperhf/copperhf_board.h.
const CHF_PROTOCOL_VERSION: u16 = 2;

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
const TD_CHANGENUM: u16 = 13;
const TD_CHANGESTATE: u16 = 14;
const TD_PROTSTATUS: u16 = 15;
const TD_GETGEOMETRY: u16 = 22;
const TD_EJECT: u16 = 23;
const TD_READ64: u16 = 24;
const TD_WRITE64: u16 = 25;
const TD_SEEK64: u16 = 26;
const TD_FORMAT64: u16 = 27;
const HD_SCSICMD: u16 = 28;
const NSCMD_TD_READ64: u16 = 0xC000;
const NSCMD_TD_WRITE64: u16 = 0xC001;
const NSCMD_TD_SEEK64: u16 = 0xC002;
const NSCMD_TD_FORMAT64: u16 = 0xC003;

// io_Error values.
const IOERR_OPENFAIL: i8 = -1;
const IOERR_NOCMD: i8 = -3;
const IOERR_BADLENGTH: i8 = -4;
const IOERR_BADADDRESS: i8 = -5;
// exec.library/io.h negative range ends at -13 (IOERR_ABORTED); trackdisk.doc
// keeps its own error numbering in the positive range starting at 20, so
// TDERR_DiskChanged is a plain positive io_Error value, not part of the
// IOERR_* family above.
const TDERR_DISK_CHANGED: i8 = 29;
// devices/scsidisk.h: HD_SCSICMD's io_Error when the target returned a
// non-GOOD scsi_Status (the request itself was delivered and answered).
const HFERR_BAD_STATUS: i8 = 45;

const SECTOR_SIZE: usize = 512;

// struct SCSICmd field offsets (devices/scsidisk.h; 32 bytes on m68k with
// natural 4-byte alignment for the pointer/ULONG fields -- 2 bytes of
// padding fall between scsi_Status and scsi_SenseData).
const SCSI_DATA: u32 = 0; // UWORD *scsi_Data
const SCSI_LENGTH: u32 = 4; // ULONG scsi_Length
const SCSI_ACTUAL: u32 = 8; // ULONG scsi_Actual
const SCSI_COMMAND: u32 = 12; // UBYTE *scsi_Command
const SCSI_CMDLENGTH: u32 = 16; // UWORD scsi_CmdLength
const SCSI_CMDACTUAL: u32 = 18; // UWORD scsi_CmdActual
/// scsi_Flags (high byte) / scsi_Status (low byte): one big-endian word.
const SCSI_FLAGS: u32 = 20;
const SCSI_SENSEDATA: u32 = 24; // UBYTE *scsi_SenseData
const SCSI_SENSELENGTH: u32 = 28; // UWORD scsi_SenseLength
const SCSI_SENSEACTUAL: u32 = 30; // UWORD scsi_SenseActual

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
    /// Each unit's backing store, wrapped in a [`ScsiDisk`] purely to reuse
    /// its CDB machinery for `HD_SCSICMD` (M4) -- there is no SCSI bus
    /// underneath; every other command talks to `.disk` directly exactly as
    /// M1-M3 did.
    units: [Option<ScsiDisk>; NUM_UNITS],
    /// `CHF_UNIT_PRESENT`: "slot configured", sticky once a unit is ever
    /// attached (boot-time `[copperhf]` config or a runtime CCP attach).
    /// Ejecting or hot-detaching a unit's media clears `units[n]` but never
    /// this -- an ejected unit stays present, like a diskless trackdisk
    /// drive (`guest/copperhf/copperhf_board.h`'s own comment on
    /// `CHF_UNIT_MEDIA`).
    present: [bool; NUM_UNITS],
    /// Disk-change counter per unit, bumped on attach/eject/detach
    /// (`CHF_CHANGE_COUNT`, `CHF_CMD_TD_CHANGENUM`).
    change_count: [u16; NUM_UNITS],
    /// `CHF_CHANGED_MASK`: bit *n* set = unit *n*'s media changed and the
    /// guest has not yet acked it via `CHF_CHANGED_ACK`.
    changed_mask: u16,
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
            present: [false; NUM_UNITS],
            change_count: [0; NUM_UNITS],
            changed_mask: 0,
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

    /// Attach a unit's image at boot time (`[copperhf]` config, before the
    /// guest has run at all): bumps the disk-change counter but does *not*
    /// set `CHF_CHANGED_MASK`.
    ///
    /// Deliberately not the same path as a runtime hot-attach
    /// ([`Self::hot_attach_unit`]): every M1-M3 guest build predates
    /// `CHF_CHANGED_MASK`/`CHF_CHANGED_ACK` entirely and never acks it, so a
    /// change flagged here would latch forever the moment the guest sets
    /// `CHF_IRQ_ENABLE` -- `CHF_IRQ_STATUS` bit 1 would stay permanently
    /// set, INT2 would never drop, and the CPU would spend the rest of the
    /// boot re-entering the interrupt handler instead of getting anywhere
    /// (verified against `tests/copperhf_device.rs`'s M2 boot, which hangs
    /// before `--run`'s staged program ever executes if this bumps
    /// `changed_mask`). A boot-time attach is not a "change" from the
    /// guest's point of view in the first place -- it is what the unit
    /// looked like when the guest first saw it.
    pub fn attach_unit(&mut self, unit: usize, image: HardDriveImage) {
        self.units[unit] = Some(ScsiDisk::from_disk(image));
        self.present[unit] = true;
        self.motor[unit] = false;
        // Deliberately does not bump change_count either: a unit that was
        // never ejected must read back CHF_CHANGE_COUNT/TD_CHANGENUM == 0
        // (guest/copperhf-test/chftest_m4.c's own test_changenum -- the
        // guest's documented contract for "this unit has never changed").
        // A boot-time attach is what the unit looked like when the guest
        // first saw it, not a change from some prior state it never had.
    }

    /// Hot-attach a unit's image at runtime (CCP `copperhf.attach`): like
    /// [`Self::attach_unit`], but also bumps the change counter and marks
    /// the change pending in `CHF_CHANGED_MASK` so a guest that is actually
    /// running (and can ack it) notices, matching the guest's own
    /// `TD_EJECT`.
    pub fn hot_attach_unit(&mut self, unit: usize, image: HardDriveImage) {
        self.attach_unit(unit, image);
        self.change_count[unit] = self.change_count[unit].wrapping_add(1);
        self.changed_mask |= 1 << unit;
    }

    /// Eject a unit's media (guest `TD_EJECT`, or a runtime CCP
    /// `copperhf.eject`/`copperhf.detach`): drops the backing image but
    /// leaves the unit present, bumps its change counter, and marks the
    /// change pending. A no-op (still bumps/flags) if the unit already had
    /// no media -- an idempotent eject is not an error. Always a runtime
    /// operation (there is no "eject" at boot-config time), so unlike
    /// [`Self::attach_unit`] this always sets `CHF_CHANGED_MASK`.
    pub fn eject_unit(&mut self, unit: usize) -> Option<HardDriveImage> {
        let image = self.units[unit].take();
        self.change_count[unit] = self.change_count[unit].wrapping_add(1);
        self.changed_mask |= 1 << unit;
        image.map(|d| d.disk)
    }

    fn present_bitmask(&self) -> u16 {
        let mut mask = 0u16;
        for (i, &p) in self.present.iter().enumerate() {
            if p {
                mask |= 1 << i;
            }
        }
        mask
    }

    fn media_bitmask(&self) -> u16 {
        let mut mask = 0u16;
        for (i, u) in self.units.iter().enumerate() {
            if u.is_some() {
                mask |= 1 << i;
            }
        }
        mask
    }

    /// Always reports every attached unit as writable, through M4.
    ///
    /// Deviation from the milestone note: there is no per-image read-only
    /// flag anywhere in the shared `HardDriveImage` layer today (see
    /// COPPERHF-DEVICE-PLAN.md's "Deferred" section) -- not even for a host
    /// disk, which exposes `is_host_disk()` but no writability query. Wiring
    /// `CHF_UNIT_RDONLY`/`TD_PROTSTATUS` up to something real is therefore a
    /// shared-layer follow-up, not something this board can honestly report
    /// on its own.
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
            .map_or(0, |img| img.disk.total_sectors() as u32)
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
            CHF_CHANGED_MASK => self.changed_mask,
            CHF_UNIT_MEDIA => self.media_bitmask(),
            CHF_IRQ_STATUS => self.irq_status(),
            CHF_IRQ_ENABLE => u16::from(self.irq_enable),
            _ => 0xFFFF,
        }
    }

    fn irq_status(&self) -> u16 {
        (u16::from(!self.completions.is_empty())) | (u16::from(self.changed_mask != 0) << 1)
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
            CHF_CHANGED_ACK => self.changed_mask &= !value,
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
            CMD_READ => self.do_rw(header, host, true, false),
            CMD_WRITE | TD_FORMAT => self.do_rw(header, host, false, false),
            CMD_UPDATE => match self.require_media(header.unit) {
                Ok(unit) => {
                    if let Err(e) = self.units[unit].as_mut().unwrap().disk.flush() {
                        log::warn!("copperhf: unit {unit}: CMD_UPDATE flush failed: {e}");
                    }
                    (0, 0)
                }
                Err(e) => (e, 0),
            },
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
            TD_CHANGENUM => match self.valid_unit(header.unit) {
                Some(unit) => (0, u32::from(self.change_count[unit])),
                None => (IOERR_OPENFAIL, 0),
            },
            TD_CHANGESTATE => match self.valid_unit(header.unit) {
                Some(unit) => (0, u32::from(self.units[unit].is_none())),
                None => (IOERR_OPENFAIL, 0),
            },
            TD_PROTSTATUS => match self.valid_unit(header.unit) {
                // rdonly_bitmask() is always 0 today (see its own doc
                // comment); mirrored here rather than hard-coded so the two
                // stay in lockstep if that ever changes.
                Some(unit) => (0, u32::from(self.rdonly_bitmask() & (1 << unit) != 0)),
                None => (IOERR_OPENFAIL, 0),
            },
            TD_EJECT => match self.valid_unit(header.unit) {
                Some(unit) => {
                    if header.length != 0 {
                        self.eject_unit(unit);
                    }
                    (0, 0)
                }
                None => (IOERR_OPENFAIL, 0),
            },
            TD_READ64 | NSCMD_TD_READ64 => self.do_rw(header, host, true, true),
            TD_WRITE64 | TD_FORMAT64 | NSCMD_TD_WRITE64 | NSCMD_TD_FORMAT64 => {
                self.do_rw(header, host, false, true)
            }
            TD_SEEK64 | NSCMD_TD_SEEK64 => match self.valid_unit(header.unit) {
                Some(_) => (0, 0),
                None => (IOERR_OPENFAIL, 0),
            },
            HD_SCSICMD => self.do_scsicmd(header, host),
            _ => (IOERR_NOCMD, 0),
        }
    }

    /// The unit index if `raw` names a present (configured) unit, `None`
    /// otherwise (out of range or an unconfigured slot). Present does not
    /// imply media -- an ejected/hot-detached unit stays present with its
    /// `CHF_UNIT_MEDIA` bit clear, like a diskless trackdisk drive.
    fn valid_unit(&self, raw: u32) -> Option<usize> {
        let unit = raw as usize;
        (unit < NUM_UNITS && self.present[unit]).then_some(unit)
    }

    /// The unit index if `raw` names a present unit that also currently has
    /// media, `Err(IOERR_OPENFAIL)` if the unit is not configured at all,
    /// `Err(TDERR_DISK_CHANGED)` if it is configured but ejected/hot-
    /// detached. I/O and geometry commands require this; the change-status
    /// trio (`TD_CHANGENUM`/`TD_CHANGESTATE`/`TD_PROTSTATUS`) and `TD_EJECT`
    /// answer from a media-absent unit too, so they use [`Self::valid_unit`]
    /// directly instead.
    fn require_media(&self, raw: u32) -> Result<usize, i8> {
        let unit = self.valid_unit(raw).ok_or(IOERR_OPENFAIL)?;
        if self.units[unit].is_none() {
            return Err(TDERR_DISK_CHANGED);
        }
        Ok(unit)
    }

    /// `CMD_READ`/`CMD_WRITE`/`TD_FORMAT` and their TD64/NSD 64-bit twins,
    /// which differ only in whether `io_Actual` carries the high 32 bits of
    /// the byte offset on entry and whether the 4 GiB address ceiling
    /// applies (`sixty_four`).
    fn do_rw(
        &mut self,
        header: &RequestHeader,
        host: &mut DeviceHost,
        read: bool,
        sixty_four: bool,
    ) -> (i8, u32) {
        let unit = match self.require_media(header.unit) {
            Ok(unit) => unit,
            Err(e) => return (e, 0),
        };
        let offset = if sixty_four {
            (u64::from(header.actual) << 32) | u64::from(header.offset)
        } else {
            u64::from(header.offset)
        };
        let (start_lba, sectors) = match self.check_range(unit, offset, header.length, !sixty_four)
        {
            Ok(range) => range,
            Err(e) => return (e, 0),
        };
        // One sector at a time -- never a whole-transfer host allocation, so
        // a full-image transfer costs 512 bytes of host buffer, not the
        // image's size, and a bad io_Data pointer fails on the first sector.
        self.activity = true;
        let image = &mut self.units[unit].as_mut().unwrap().disk;
        if read {
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
        } else {
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
        }
        (0, header.length)
    }

    /// Validate a transfer's alignment and range against a unit's size,
    /// returning `(start_lba, sector_count)` on success. `cap_4gib` applies
    /// the plain 32-bit commands' no-wrap rule
    /// (`guest/copperhf/copperhf_board.h`): `offset + length` reaching past
    /// the 4 GiB boundary is `IOERR_BADADDRESS`, distinct from an ordinary
    /// past-end-of-unit `IOERR_BADLENGTH`. The 64-bit commands never set it,
    /// since a 64-bit offset addresses well past 4 GiB legitimately.
    fn check_range(
        &self,
        unit: usize,
        offset: u64,
        length: u32,
        cap_4gib: bool,
    ) -> Result<(u64, u64), i8> {
        if length == 0
            || !(length as usize).is_multiple_of(SECTOR_SIZE)
            || !(offset as usize).is_multiple_of(SECTOR_SIZE)
        {
            return Err(IOERR_BADLENGTH);
        }
        let end = offset
            .checked_add(u64::from(length))
            .ok_or(IOERR_BADADDRESS)?;
        if cap_4gib && end > u64::from(u32::MAX) + 1 {
            return Err(IOERR_BADADDRESS);
        }
        let total_bytes = self.units[unit]
            .as_ref()
            .ok_or(IOERR_OPENFAIL)?
            .disk
            .total_sectors()
            * SECTOR_SIZE as u64;
        if end > total_bytes {
            return Err(IOERR_BADLENGTH);
        }
        let start_lba = offset / SECTOR_SIZE as u64;
        let sectors = u64::from(length) / SECTOR_SIZE as u64;
        Ok((start_lba, sectors))
    }

    fn do_get_geometry(&mut self, header: &RequestHeader, host: &mut DeviceHost) -> (i8, u32) {
        let unit = match self.require_media(header.unit) {
            Ok(unit) => unit,
            Err(e) => return (e, 0),
        };
        if header.length < 32 {
            return (IOERR_BADLENGTH, 0);
        }
        let total_sectors = self.units[unit].as_ref().unwrap().disk.total_sectors() as u32;
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

    /// `HD_SCSICMD`: `io_Data` points at a `struct SCSICmd` (32 bytes on
    /// m68k -- `devices/scsidisk.h`; field offsets below confirmed against
    /// the NDK autodocs). This board answers it against the unit's own
    /// image without a real SCSI bus underneath, reusing
    /// `src/scsi.rs::ScsiDisk`'s CDB machinery (the same target model the
    /// A2091/A4091 boards drive over the WD33C93A).
    fn do_scsicmd(&mut self, header: &RequestHeader, host: &mut DeviceHost) -> (i8, u32) {
        let unit = match self.require_media(header.unit) {
            Ok(unit) => unit,
            Err(e) => return (e, 0),
        };
        let Some(cmd) = ScsiCmdHeader::read(header.data, host) else {
            return (IOERR_BADADDRESS, 0);
        };
        let cdb_len = (cmd.cmd_length as usize).min(16);
        let Some(cdb) = dma_read_bytes(host, cmd.command, cdb_len) else {
            return (IOERR_BADADDRESS, 0);
        };

        self.activity = true;
        let disk = self.units[unit].as_mut().unwrap();
        let (exec, mut status) = disk.execute(&cdb, 0);
        let data_len = match exec {
            ScsiExec::DataIn(data) => {
                let n = data.len().min(cmd.length as usize);
                if !dma_write_bytes(host, cmd.data, &data[..n]) {
                    return (IOERR_BADADDRESS, 0);
                }
                n
            }
            ScsiExec::DataOut(expected) => {
                let n = expected.min(cmd.length as usize);
                let Some(mut payload) = dma_read_bytes(host, cmd.data, n) else {
                    return (IOERR_BADADDRESS, 0);
                };
                payload.resize(expected, 0);
                status = self.units[unit]
                    .as_mut()
                    .unwrap()
                    .complete_out(&cdb, &payload);
                n
            }
            ScsiExec::NoData => 0,
        };

        let sense = (status == CHECK_CONDITION)
            .then(|| self.units[unit].as_ref().unwrap().sense_bytes().to_vec());

        // scsi_Actual (u32 @8), scsi_CmdActual (u16 @18), scsi_Flags/Status
        // (one big-endian word @20: high byte is scsi_Flags, which is
        // preserved rather than overwritten with zero).
        let flags_word = host.dma_read_word(header.data + SCSI_FLAGS).unwrap_or(0);
        let ok = write_u32(host, header.data + SCSI_ACTUAL, data_len as u32)
            && write_u16(host, header.data + SCSI_CMDACTUAL, cdb_len as u16)
            && host.dma_write_word(
                header.data + SCSI_FLAGS,
                (flags_word & 0xFF00) | u16::from(status),
            );
        if !ok {
            return (IOERR_BADADDRESS, 0);
        }
        if let Some(sense) = sense {
            self.write_scsi_sense(header.data, &cmd, &sense, host);
        }
        // scsi.device's documented HD_SCSICMD contract: io_Error reports
        // HFERR_BadStatus whenever scsi_Status came back non-GOOD (the CDB
        // itself was delivered fine -- the target just rejected it), so a
        // caller that only checks io_Error still notices the failure.
        if status != crate::scsi::GOOD {
            return (HFERR_BAD_STATUS, 0);
        }
        (0, 0)
    }

    /// Fill `scsi_SenseData`/`scsi_SenseActual` after a CHECK CONDITION,
    /// honouring `scsi_SenseLength` and `SCSIF_AUTOSENSE`/`SCSIF_OLDAUTOSENSE`
    /// (`scsi_Flags` bits 1/2 -- `SCSIB_AUTOSENSE`/`SCSIB_OLDAUTOSENSE`).
    /// Silent (leaves `scsi_SenseActual` at whatever the guest initialized
    /// it to) if autosense was not requested or the sense pointer cannot be
    /// reached -- a `SCSICmd` without autosense has no sense buffer to write
    /// into at all.
    fn write_scsi_sense(
        &self,
        req_data: u32,
        cmd: &ScsiCmdHeader,
        sense: &[u8],
        host: &mut DeviceHost,
    ) {
        if cmd.flags & 0b0110 == 0 {
            return;
        }
        let Some(sense_ptr) = read_u32(host, req_data + SCSI_SENSEDATA) else {
            return;
        };
        if sense_ptr == 0 {
            return;
        }
        let n = (cmd.sense_length as usize).min(sense.len());
        if n > 0 && !dma_write_bytes(host, sense_ptr, &sense[..n]) {
            return;
        }
        let _ = write_u16(host, req_data + SCSI_SENSEACTUAL, n as u16);
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
    /// `io_Actual` as read on entry: unused input for every M1-M3 command,
    /// but the TD64/NSD 64-bit commands alias it as `io_HighOffset` (the
    /// upper 32 bits of a 64-bit byte offset, `devices/newstyle.h`) --
    /// `io_Actual` is still an *output* on completion for every command
    /// including these, so `execute_request` always overwrites it with the
    /// transfer count afterwards.
    actual: u32,
}

impl RequestHeader {
    fn read(ptr: u32, host: &DeviceHost) -> Option<Self> {
        let unit = read_u32(host, ptr + IO_UNIT)?;
        let command = host.dma_read_word(ptr + IO_COMMAND)?;
        let flags = (host.dma_read_word(ptr + IO_FLAGS_ERROR)? >> 8) as u8;
        let actual = read_u32(host, ptr + IO_ACTUAL)?;
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
            actual,
        })
    }
}

/// The IOStdReq fields `HD_SCSICMD` reads out of the guest's `struct
/// SCSICmd` (see the `SCSI_*` offset constants above). `scsi_Actual` is not
/// read here: it is an output-only field this device always overwrites.
struct ScsiCmdHeader {
    data: u32,
    length: u32,
    command: u32,
    cmd_length: u16,
    flags: u8,
    sense_length: u16,
}

impl ScsiCmdHeader {
    fn read(ptr: u32, host: &DeviceHost) -> Option<Self> {
        let data = read_u32(host, ptr + SCSI_DATA)?;
        let length = read_u32(host, ptr + SCSI_LENGTH)?;
        let command = read_u32(host, ptr + SCSI_COMMAND)?;
        let cmd_length = host.dma_read_word(ptr + SCSI_CMDLENGTH)?;
        let flags_status = host.dma_read_word(ptr + SCSI_FLAGS)?;
        let sense_length = host.dma_read_word(ptr + SCSI_SENSELENGTH)?;
        Some(Self {
            data,
            length,
            command,
            cmd_length,
            flags: (flags_status >> 8) as u8,
            sense_length,
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

fn write_u16(host: &mut DeviceHost, addr: u32, value: u16) -> bool {
    host.dma_write_word(addr, value)
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

/// One byte of the ROM window: the actual ROM byte if `off` (window-
/// relative) falls inside `ROM_OFFSET..ROM_OFFSET + COPPERHF_ROM.len()`,
/// otherwise `0xFF` -- matching every other unmapped offset on this board
/// (`read_word`/`read_long`'s own `0xFFFF`/`0xFFFF_FFFF` fallthrough), so a
/// read that straddles the ROM's end or probes the unused bytes ahead of
/// `ROM_OFFSET` reads as the same "nothing here" pattern instead of
/// panicking or wrapping into unrelated register offsets.
fn rom_byte(off: u32) -> u8 {
    (off as usize)
        .checked_sub(ROM_OFFSET)
        .and_then(|i| COPPERHF_ROM.get(i))
        .copied()
        .unwrap_or(0xFF)
}

/// A big-endian `size`-byte (1, 2, or 4) read assembled from [`rom_byte`],
/// for any window offset in `0..ROM_WINDOW_END`.
fn rom_read(off: u32, size: usize) -> u32 {
    (0..size as u32).fold(0, |acc, i| (acc << 8) | u32::from(rom_byte(off + i)))
}

impl ZorroDevice for CopperhfBoard {
    fn read(&mut self, off: u32, size: usize, _host: &mut DeviceHost) -> u32 {
        if off < ROM_WINDOW_END {
            return rom_read(off, size);
        }
        match size {
            4 => self.read_long(off),
            2 => u32::from(self.read_word(off)),
            _ => 0xFFFF_FFFF,
        }
    }

    fn write(&mut self, off: u32, size: usize, value: u32, host: &mut DeviceHost) {
        // The ROM window is read-only: writes below CHF_MAGIC are silently
        // dropped, same as a real write-protected boot ROM.
        if off < ROM_WINDOW_END {
            return;
        }
        match size {
            4 => self.write_long(off, value, host),
            2 => self.write_word(off, value as u16, host),
            _ => {}
        }
    }

    fn peek_word(&self, off: u32) -> Option<u16> {
        if off < ROM_WINDOW_END {
            return Some(rom_read(off, 2) as u16);
        }
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
            CHF_CHANGED_MASK => Some(self.changed_mask),
            CHF_UNIT_MEDIA => Some(self.media_bitmask()),
            CHF_COMPLETE_GET => Some((self.completions.front().copied().unwrap_or(0) >> 16) as u16),
            _ if off == CHF_COMPLETE_GET + 2 => {
                Some(self.completions.front().copied().unwrap_or(0) as u16)
            }
            CHF_IRQ_STATUS => Some(self.irq_status()),
            CHF_IRQ_ENABLE => Some(u16::from(self.irq_enable)),
            _ => None,
        }
    }

    fn tick(&mut self, _cck: u32, _host: &mut DeviceHost) {
        // M1 is fully synchronous: nothing to advance between register
        // accesses. The worker-thread cadence lands in M5.
    }

    fn int2_line(&self) -> bool {
        self.irq_enable && self.irq_status() != 0
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

    /// Pokes `io_Actual` *before* ringing the doorbell -- the TD64/NSD 64-bit
    /// commands alias it as `io_HighOffset` (the upper 32 bits of the byte
    /// offset) on entry, unlike every other command's `io_Actual`, which is
    /// output-only.
    fn set_io_high_offset(mem: &mut Memory, ptr: u32, high: u32) {
        mem.chip_ram[ptr as usize + IO_ACTUAL as usize..][..4].copy_from_slice(&high.to_be_bytes());
    }

    /// A sparse flat image (no bare-partition/RDB sniffing applies -- the
    /// file starts all zero) sized `total_bytes`, allocated with `set_len`
    /// so a multi-GiB fixture costs no real disk space on a filesystem that
    /// supports sparse files (APFS/ext4/...).
    fn sparse_image(name: &str, total_bytes: u64) -> HardDriveImage {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "copperline-copperhf-sparse-{}-{unique}-{name}",
            std::process::id()
        ));
        std::fs::File::create(&path)
            .unwrap()
            .set_len(total_bytes)
            .unwrap();
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

    /// Write a `struct SCSICmd` (see the `SCSI_*` offset constants) into
    /// guest chip RAM at `ptr`. `scsi_Actual`/`scsi_CmdActual`/`scsi_Status`/
    /// `scsi_SenseActual` are left at zero -- they are output fields the
    /// board fills in.
    #[allow(clippy::too_many_arguments)]
    fn write_scsicmd(
        mem: &mut Memory,
        ptr: u32,
        data: u32,
        length: u32,
        command: u32,
        cmd_length: u16,
        flags: u8,
        sense_data: u32,
        sense_length: u16,
    ) {
        let p = ptr as usize;
        mem.chip_ram[p + SCSI_DATA as usize..][..4].copy_from_slice(&data.to_be_bytes());
        mem.chip_ram[p + SCSI_LENGTH as usize..][..4].copy_from_slice(&length.to_be_bytes());
        mem.chip_ram[p + SCSI_COMMAND as usize..][..4].copy_from_slice(&command.to_be_bytes());
        mem.chip_ram[p + SCSI_CMDLENGTH as usize..][..2].copy_from_slice(&cmd_length.to_be_bytes());
        mem.chip_ram[p + SCSI_FLAGS as usize] = flags; // high byte of the word; status (low) stays 0
        mem.chip_ram[p + SCSI_SENSEDATA as usize..][..4].copy_from_slice(&sense_data.to_be_bytes());
        mem.chip_ram[p + SCSI_SENSELENGTH as usize..][..2]
            .copy_from_slice(&sense_length.to_be_bytes());
    }

    fn scsi_status(mem: &Memory, scsicmd_ptr: u32) -> u8 {
        mem.chip_ram[scsicmd_ptr as usize + SCSI_FLAGS as usize + 1]
    }

    fn scsi_actual(mem: &Memory, scsicmd_ptr: u32) -> u32 {
        u32::from_be_bytes(
            mem.chip_ram[scsicmd_ptr as usize + SCSI_ACTUAL as usize..][..4]
                .try_into()
                .unwrap(),
        )
    }

    fn scsi_sense_actual(mem: &Memory, scsicmd_ptr: u32) -> u16 {
        u16::from_be_bytes(
            mem.chip_ram[scsicmd_ptr as usize + SCSI_SENSEACTUAL as usize..][..2]
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
    fn unit_present_bitmask_reflects_attach_and_stays_set_after_eject() {
        // M4: CHF_UNIT_PRESENT is "slot configured", sticky once attached --
        // ejecting only clears CHF_UNIT_MEDIA (see the eject/media test
        // below), like a diskless trackdisk drive.
        let mut board = CopperhfBoard::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(CHF_UNIT_PRESENT, 2, &mut host), 0);
        board.attach_unit(2, open_image("present", 16));
        assert_eq!(board.read(CHF_UNIT_PRESENT, 2, &mut host), 0b100);
        board.eject_unit(2);
        assert_eq!(board.read(CHF_UNIT_PRESENT, 2, &mut host), 0b100);
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
    fn boot_time_attach_leaves_change_count_at_zero() {
        // guest/copperhf-test/chftest_m4.c's test_changenum: "unit 0 has
        // never been ejected, so the change counter must read back 0" --
        // a boot-time [copperhf] config attach is not itself a change.
        let mut board = CopperhfBoard::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(CHF_UNIT_SELECT, 2, 0, &mut host);
        assert_eq!(board.read(CHF_CHANGE_COUNT, 2, &mut host), 0);
        board.attach_unit(0, open_image("changecount-boot", 16));
        assert_eq!(board.read(CHF_CHANGE_COUNT, 2, &mut host), 0);
    }

    #[test]
    fn change_count_bumps_on_hot_attach_and_eject() {
        let mut board = CopperhfBoard::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(CHF_UNIT_SELECT, 2, 0, &mut host);
        assert_eq!(board.read(CHF_CHANGE_COUNT, 2, &mut host), 0);
        board.hot_attach_unit(0, open_image("changecount", 16));
        assert_eq!(board.read(CHF_CHANGE_COUNT, 2, &mut host), 1);
        board.eject_unit(0);
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

    // -- M2: boot ROM serving -------------------------------------------

    #[test]
    fn rom_is_readable_at_window_offset_zero_and_matches_the_included_bytes() {
        let mut board = CopperhfBoard::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        // Window offset 0 is below ROM_OFFSET (the unused bytes ahead of
        // the entry table on this board, unlike filesys's fake seglist
        // header there) -- reads as the unmapped 0xFF pattern.
        assert_eq!(board.read(0, 2, &mut host), 0xFFFF);
        for (i, chunk) in COPPERHF_ROM.chunks(2).enumerate() {
            let off = ROM_OFFSET as u32 + (i as u32) * 2;
            let expected = if chunk.len() == 2 {
                u16::from_be_bytes([chunk[0], chunk[1]])
            } else {
                u16::from(chunk[0]) << 8 | 0xFF
            };
            assert_eq!(
                board.read(off, 2, &mut host) as u16,
                expected,
                "mismatch at ROM offset {i}"
            );
        }
    }

    #[test]
    fn rom_peek_word_matches_read_and_has_no_side_effects() {
        let board = CopperhfBoard::new();
        let expected = u16::from_be_bytes([COPPERHF_ROM[0], COPPERHF_ROM[1]]);
        assert_eq!(
            ZorroDevice::peek_word(&board, ROM_OFFSET as u32),
            Some(expected)
        );
    }

    #[test]
    fn rom_reads_past_its_end_but_below_the_register_block_are_unmapped() {
        let mut board = CopperhfBoard::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        let past_end = ROM_OFFSET as u32 + COPPERHF_ROM.len() as u32;
        assert!(
            past_end < ROM_WINDOW_END,
            "fixture assumes ROM has headroom"
        );
        assert_eq!(board.read(past_end, 2, &mut host), 0xFFFF);
        assert_eq!(board.read(ROM_WINDOW_END - 2, 4, &mut host), 0xFFFF_FFFF);
    }

    #[test]
    fn rom_writes_are_silently_ignored() {
        let mut board = CopperhfBoard::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        let before = board.read(ROM_OFFSET as u32, 2, &mut host);
        board.write(ROM_OFFSET as u32, 2, 0xDEAD, &mut host);
        assert_eq!(board.read(ROM_OFFSET as u32, 2, &mut host), before);
    }

    #[test]
    fn diag_offset_points_at_a_diagarea_with_a_nonzero_bootpoint() {
        // DIAG_OFFSET must land inside the ROM (not past its end) and its
        // da_Config byte must carry DAC_WORDWIDE | DAC_CONFIGTIME (0x90),
        // matching entry.s's `_diag_area` -- DAC_CONFIGTIME additionally
        // requires a non-zero da_BootPoint, the hard-won Kickstart 3.x trap
        // entry.s's own header comment documents.
        let d = DIAG_OFFSET as usize - ROM_OFFSET; // ROM-file-relative
        assert!(d + 10 <= COPPERHF_ROM.len());
        assert_eq!(
            COPPERHF_ROM[d], 0x90,
            "da_Config must be DAC_WORDWIDE|DAC_CONFIGTIME"
        );
        let da_size = u16::from_be_bytes([COPPERHF_ROM[d + 2], COPPERHF_ROM[d + 3]]) as usize;
        let da_boot = u16::from_be_bytes([COPPERHF_ROM[d + 6], COPPERHF_ROM[d + 7]]) as usize;
        assert!(
            da_boot != 0 && da_boot < da_size,
            "da_BootPoint must be non-zero"
        );
        assert!(d + da_size <= COPPERHF_ROM.len());
    }

    #[test]
    fn register_block_still_works_with_the_rom_present() {
        // The ROM's addition must not disturb the M1 register protocol at
        // all: identity registers and a full read/write round trip both
        // still behave exactly as the M1 tests above already assert.
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("rom-coexist", 16));
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(CHF_MAGIC, 4, &mut host), CHF_MAGIC_VALUE);

        let ptr = 0x1000u32;
        let data_addr = 0x2000u32;
        write_request(&mut mem, ptr, 0, CMD_READ, 0, 512, data_addr, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), 0);
        assert_eq!(io_actual(&mem, ptr), 512);
    }

    // -- M4: command coverage --------------------------------------------

    #[test]
    fn version_register_reports_2() {
        let mut board = CopperhfBoard::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(CHF_VERSION, 2, &mut host), 2);
    }

    #[test]
    fn boot_time_attach_does_not_flag_a_change() {
        // A boot-time [copperhf] config attach happens before any guest
        // has run: no M1-M3 guest ever acks CHF_CHANGED_MASK, so flagging
        // one here would latch INT2 forever the moment CHF_IRQ_ENABLE gets
        // set (this exact bug hung tests/copperhf_device.rs's AROS boot
        // before its --run payload ever executed).
        let mut board = CopperhfBoard::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.attach_unit(1, open_image("boot-attach", 16));
        assert_eq!(board.read(CHF_UNIT_MEDIA, 2, &mut host), 0b10);
        assert_eq!(board.read(CHF_CHANGED_MASK, 2, &mut host), 0);
        assert_eq!(board.read(CHF_IRQ_STATUS, 2, &mut host), 0);
    }

    #[test]
    fn hot_attach_sets_media_and_changed_bit_and_ack_clears_it() {
        let mut board = CopperhfBoard::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(CHF_UNIT_MEDIA, 2, &mut host), 0);
        assert_eq!(board.read(CHF_CHANGED_MASK, 2, &mut host), 0);

        board.hot_attach_unit(1, open_image("changed-attach", 16));
        assert_eq!(board.read(CHF_UNIT_MEDIA, 2, &mut host), 0b10);
        assert_eq!(board.read(CHF_CHANGED_MASK, 2, &mut host), 0b10);
        assert_eq!(board.read(CHF_IRQ_STATUS, 2, &mut host), 0b10);

        board.write(CHF_CHANGED_ACK, 2, 0b10, &mut host);
        assert_eq!(board.read(CHF_CHANGED_MASK, 2, &mut host), 0);
        assert_eq!(board.read(CHF_IRQ_STATUS, 2, &mut host), 0);
    }

    #[test]
    fn changed_ack_only_clears_the_acked_bits() {
        let mut board = CopperhfBoard::new();
        board.hot_attach_unit(0, open_image("ack-0", 16));
        board.hot_attach_unit(1, open_image("ack-1", 16));
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(CHF_CHANGED_MASK, 2, &mut host), 0b11);
        board.write(CHF_CHANGED_ACK, 2, 0b01, &mut host);
        assert_eq!(board.read(CHF_CHANGED_MASK, 2, &mut host), 0b10);
    }

    #[test]
    fn td_eject_clears_media_keeps_present_and_flags_change() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("eject", 16)); // boot-time: no changed bit yet
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(CHF_CHANGED_MASK, 2, &mut host), 0);

        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, TD_EJECT, 0, 1, 0, 0); // io_Length != 0
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(board.read(CHF_UNIT_PRESENT, 2, &mut host), 0b1);
        assert_eq!(board.read(CHF_UNIT_MEDIA, 2, &mut host), 0);
        assert_eq!(board.read(CHF_CHANGED_MASK, 2, &mut host), 0b1);
        assert_eq!(io_error(&mem, ptr), 0);
    }

    #[test]
    fn td_eject_with_zero_length_is_a_no_op_insert() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("eject-noop", 16));
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(CHF_CHANGED_ACK, 2, 0xFFFF, &mut host);

        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, TD_EJECT, 0, 0, 0, 0); // io_Length == 0
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(
            board.read(CHF_UNIT_MEDIA, 2, &mut host),
            0b1,
            "still has media"
        );
        assert_eq!(
            board.read(CHF_CHANGED_MASK, 2, &mut host),
            0,
            "no change flagged"
        );
        assert_eq!(io_error(&mem, ptr), 0);
    }

    #[test]
    fn td_changenum_changestate_protstatus_round_trip() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("tdstatus", 16));
        let mut mem = memory();

        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, TD_CHANGENUM, 0, 0, 0, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), 0);
        assert_eq!(io_actual(&mem, ptr), 0, "boot-time attach is not a change");

        let ptr2 = 0x1100u32;
        write_request(&mut mem, ptr2, 0, TD_CHANGESTATE, 0, 0, 0, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr2);
        assert_eq!(io_error(&mem, ptr2), 0);
        assert_eq!(io_actual(&mem, ptr2), 0, "media present");

        let ptr3 = 0x1200u32;
        write_request(&mut mem, ptr3, 0, TD_PROTSTATUS, 0, 0, 0, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr3);
        assert_eq!(io_error(&mem, ptr3), 0);
        assert_eq!(io_actual(&mem, ptr3), 0, "writable");
    }

    #[test]
    fn media_absent_unit_fails_io_but_answers_change_status() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("absent", 16));
        board.eject_unit(0);
        let mut mem = memory();

        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, CMD_READ, 0, 512, 0x2000, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), TDERR_DISK_CHANGED);

        let ptr2 = 0x1100u32;
        write_request(&mut mem, ptr2, 0, TD_GETGEOMETRY, 0, 32, 0x2000, 0);
        let mut host2 = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host2, ptr2);
        assert_eq!(io_error(&mem, ptr2), TDERR_DISK_CHANGED);

        // CHANGENUM/CHANGESTATE/PROTSTATUS still answer from a present-but-
        // media-absent unit.
        let ptr3 = 0x1200u32;
        write_request(&mut mem, ptr3, 0, TD_CHANGESTATE, 0, 0, 0, 0);
        let mut host3 = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host3, ptr3);
        assert_eq!(io_error(&mem, ptr3), 0);
        assert_eq!(io_actual(&mem, ptr3), 1, "media absent");
    }

    #[test]
    fn plain_cmd_read_offset_length_overflowing_u32_is_bad_address() {
        // A disk bigger than 4 GiB, so the ordinary past-end-of-unit bound
        // (IOERR_BADLENGTH) does not mask the no-wrap rule.
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, sparse_image("overflow", 5 * (1u64 << 30)));
        let mut mem = memory();
        let ptr = 0x1000u32;
        // offset + length overflows u32 (offset alone is already the max
        // 32-bit-aligned sector-multiple value below 4 GiB).
        write_request(&mut mem, ptr, 0, CMD_READ, 0, 1024, 0x2000, 0xFFFF_FE00);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), IOERR_BADADDRESS);
    }

    #[test]
    fn td64_write_then_read_round_trips_past_4gib_on_a_sparse_image() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, sparse_image("td64", 5 * (1u64 << 30)));
        let mut mem = memory();

        let offset: u64 = (1u64 << 32) + 512; // just past the 4 GiB boundary
        let pattern: Vec<u8> = (0..512u32).map(|i| (i * 7 + 3) as u8).collect();
        let data_addr = 0x2000u32;
        mem.chip_ram[data_addr as usize..data_addr as usize + 512].copy_from_slice(&pattern);

        let ptr = 0x1000u32;
        write_request(
            &mut mem,
            ptr,
            0,
            TD_WRITE64,
            0,
            512,
            data_addr,
            offset as u32,
        );
        set_io_high_offset(&mut mem, ptr, (offset >> 32) as u32);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), 0);
        assert_eq!(io_actual(&mem, ptr), 512);

        let ptr2 = 0x1100u32;
        let readback_addr = 0x3000u32;
        write_request(
            &mut mem,
            ptr2,
            0,
            TD_READ64,
            0,
            512,
            readback_addr,
            offset as u32,
        );
        set_io_high_offset(&mut mem, ptr2, (offset >> 32) as u32);
        let mut host2 = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host2, ptr2);
        assert_eq!(io_error(&mem, ptr2), 0);
        assert_eq!(
            &mem.chip_ram[readback_addr as usize..readback_addr as usize + 512],
            &pattern[..]
        );
    }

    #[test]
    fn nscmd_td_read64_behaves_like_td_read64() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, sparse_image("nscmd64", 5 * (1u64 << 30)));
        let mut mem = memory();
        let offset: u64 = 3 * (1u64 << 30); // 3 GiB: below 4 GiB, still exercises the path

        let ptr = 0x1000u32;
        let data_addr = 0x2000u32;
        write_request(
            &mut mem,
            ptr,
            0,
            NSCMD_TD_READ64,
            0,
            512,
            data_addr,
            offset as u32,
        );
        set_io_high_offset(&mut mem, ptr, (offset >> 32) as u32);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), 0);
        assert_eq!(io_actual(&mem, ptr), 512);
    }

    #[test]
    fn td_seek64_is_a_success_no_op() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("seek64", 16));
        let mut mem = memory();
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, TD_SEEK64, 0, 0, 0, 0);
        set_io_high_offset(&mut mem, ptr, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), 0);
    }

    #[test]
    fn hd_scsicmd_inquiry_round_trips_identity() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("scsi-inquiry", 16));
        let mut mem = memory();

        let cdb_addr = 0x1800u32;
        let cdb = [0x12u8, 0, 0, 0, 36, 0];
        mem.chip_ram[cdb_addr as usize..cdb_addr as usize + cdb.len()].copy_from_slice(&cdb);
        let data_addr = 0x2000u32;
        let scsicmd_ptr = 0x1400u32;
        write_scsicmd(
            &mut mem,
            scsicmd_ptr,
            data_addr,
            64,
            cdb_addr,
            cdb.len() as u16,
            0,
            0,
            0,
        );
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, HD_SCSICMD, 0, 0, scsicmd_ptr, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);

        assert_eq!(io_error(&mem, ptr), 0);
        assert_eq!(scsi_status(&mem, scsicmd_ptr), 0, "GOOD");
        assert_eq!(scsi_actual(&mem, scsicmd_ptr), 36);
        assert_eq!(
            &mem.chip_ram[data_addr as usize + 8..data_addr as usize + 16],
            b"COPPERLN"
        );
    }

    #[test]
    fn hd_scsicmd_read_capacity10_reports_last_lba_and_block_size() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("scsi-capacity", 16));
        let mut mem = memory();

        let cdb_addr = 0x1800u32;
        let cdb = [0x25u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        mem.chip_ram[cdb_addr as usize..cdb_addr as usize + cdb.len()].copy_from_slice(&cdb);
        let data_addr = 0x2000u32;
        let scsicmd_ptr = 0x1400u32;
        write_scsicmd(
            &mut mem,
            scsicmd_ptr,
            data_addr,
            8,
            cdb_addr,
            cdb.len() as u16,
            0,
            0,
            0,
        );
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, HD_SCSICMD, 0, 0, scsicmd_ptr, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);

        assert_eq!(io_error(&mem, ptr), 0);
        assert_eq!(scsi_actual(&mem, scsicmd_ptr), 8);
        let last = u32::from_be_bytes(
            mem.chip_ram[data_addr as usize..data_addr as usize + 4]
                .try_into()
                .unwrap(),
        );
        let block_size = u32::from_be_bytes(
            mem.chip_ram[data_addr as usize + 4..data_addr as usize + 8]
                .try_into()
                .unwrap(),
        );
        assert_eq!(last, 15, "16 sectors -> last LBA 15");
        assert_eq!(block_size, 512);
    }

    #[test]
    fn hd_scsicmd_read10_returns_the_units_sector_data() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("scsi-read10", 16));
        let mut mem = memory();

        let cdb_addr = 0x1800u32;
        // READ(10): lba 0, transfer length 1.
        let cdb = [0x28u8, 0, 0, 0, 0, 0, 0, 0, 1, 0];
        mem.chip_ram[cdb_addr as usize..cdb_addr as usize + cdb.len()].copy_from_slice(&cdb);
        let data_addr = 0x2000u32;
        let scsicmd_ptr = 0x1400u32;
        write_scsicmd(
            &mut mem,
            scsicmd_ptr,
            data_addr,
            512,
            cdb_addr,
            cdb.len() as u16,
            0,
            0,
            0,
        );
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, HD_SCSICMD, 0, 0, scsicmd_ptr, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);

        assert_eq!(io_error(&mem, ptr), 0);
        assert_eq!(scsi_actual(&mem, scsicmd_ptr), 512);
        let expected: Vec<u8> = (0..512u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(
            &mem.chip_ram[data_addr as usize..data_addr as usize + 512],
            &expected[..]
        );
    }

    #[test]
    fn hd_scsicmd_check_condition_autosense_fills_sense_data() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("scsi-sense", 16));
        let mut mem = memory();

        let cdb_addr = 0x1800u32;
        let cdb = [0xFFu8, 0, 0, 0, 0, 0]; // unsupported opcode
        mem.chip_ram[cdb_addr as usize..cdb_addr as usize + cdb.len()].copy_from_slice(&cdb);
        let sense_addr = 0x2400u32;
        let scsicmd_ptr = 0x1400u32;
        write_scsicmd(
            &mut mem,
            scsicmd_ptr,
            0,
            0,
            cdb_addr,
            cdb.len() as u16,
            0b0010, // SCSIF_AUTOSENSE
            sense_addr,
            18,
        );
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, HD_SCSICMD, 0, 0, scsicmd_ptr, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);

        assert_eq!(
            io_error(&mem, ptr),
            HFERR_BAD_STATUS,
            "non-GOOD scsi_Status reports HFERR_BadStatus in io_Error \
             (scsi.device's documented HD_SCSICMD contract)"
        );
        assert_eq!(scsi_status(&mem, scsicmd_ptr), 0x02, "CHECK CONDITION");
        assert_eq!(scsi_sense_actual(&mem, scsicmd_ptr), 18);
        assert_eq!(mem.chip_ram[sense_addr as usize], 0x70, "current error");
        assert_eq!(
            mem.chip_ram[sense_addr as usize + 2],
            0x05,
            "ILLEGAL_REQUEST"
        );
        assert_eq!(
            mem.chip_ram[sense_addr as usize + 12],
            0x20,
            "INVALID_OPCODE"
        );
    }

    #[test]
    fn hd_scsicmd_without_autosense_leaves_sense_actual_untouched() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("scsi-nosense", 16));
        let mut mem = memory();

        let cdb_addr = 0x1800u32;
        let cdb = [0xFFu8, 0, 0, 0, 0, 0];
        mem.chip_ram[cdb_addr as usize..cdb_addr as usize + cdb.len()].copy_from_slice(&cdb);
        let sense_addr = 0x2400u32;
        let scsicmd_ptr = 0x1400u32;
        write_scsicmd(
            &mut mem,
            scsicmd_ptr,
            0,
            0,
            cdb_addr,
            cdb.len() as u16,
            0, // SCSIF_NOSENSE
            sense_addr,
            18,
        );
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, HD_SCSICMD, 0, 0, scsicmd_ptr, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);

        assert_eq!(io_error(&mem, ptr), HFERR_BAD_STATUS);
        assert_eq!(scsi_status(&mem, scsicmd_ptr), 0x02, "CHECK CONDITION");
        assert_eq!(
            scsi_sense_actual(&mem, scsicmd_ptr),
            0,
            "no autosense requested"
        );
    }

    #[test]
    fn savestate_round_trip_keeps_media_present_and_changed_state() {
        // Unit 0: attached (boot-time, no changed bit), then ejected
        // (present, media-absent, changed flagged). Unit 1: attached
        // boot-time only (present, media, no changed bit -- boot-time
        // attach never flags a change). Unit 2: never configured at all.
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("state-0", 16));
        board.eject_unit(0);
        board.attach_unit(1, open_image("state-1", 16));

        let encoded = bincode::serialize(&board).unwrap();
        let mut restored: CopperhfBoard = bincode::deserialize(&encoded).unwrap();

        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(restored.read(CHF_UNIT_PRESENT, 2, &mut host), 0b011);
        assert_eq!(restored.read(CHF_UNIT_MEDIA, 2, &mut host), 0b010);
        assert_eq!(restored.read(CHF_CHANGED_MASK, 2, &mut host), 0b001);

        // The hot-attached unit 1's backing image survived intact.
        restored.write(CHF_UNIT_SELECT, 2, 1, &mut host);
        assert_eq!(restored.read(CHF_UNIT_BLOCKS, 4, &mut host), 16);
    }

    #[test]
    fn hd_scsicmd_against_media_absent_unit_fails_with_disk_changed() {
        let mut board = CopperhfBoard::new();
        board.attach_unit(0, open_image("scsi-absent", 16));
        board.eject_unit(0);
        let mut mem = memory();

        let scsicmd_ptr = 0x1400u32;
        let ptr = 0x1000u32;
        write_request(&mut mem, ptr, 0, HD_SCSICMD, 0, 0, scsicmd_ptr, 0);
        let mut host = DeviceHost::new(&mut mem);
        ring_doorbell_long(&mut board, &mut host, ptr);
        assert_eq!(io_error(&mem, ptr), TDERR_DISK_CHANGED);
    }
}
