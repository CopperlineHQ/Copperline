// SPDX-License-Identifier: GPL-3.0-or-later

//! Commodore A4091: a Zorro III SCSI-2 controller carrying an NCR 53C710
//! and a nibble-wide autoboot ROM.
//!
//! Board layout (offsets within the configured 16M window, matching the
//! A4091 schematics and the WinUAE/Amiberry emulation):
//!
//! - `$000000-$7FFFFF` the boot ROM, nibble-wide: ROM byte `i` presents its
//!   high nibble at window offset `i*4` and its low nibble at `i*4+2` (both
//!   with the low nibble of the lane forced to `$F`); odd offsets float
//!   `$FF`. expansion.library reassembles the DiagArea from the nibbles
//!   (DAC_NIBBLEWIDE), and the ROM's own relocator copies the driver the
//!   same way. A 32K image mirrors to fill the 64K ROM space.
//! - `$800000-$87FFFF` the 53C710 registers. The board decodes only the
//!   low 6 address bits, so the 64-byte register file mirrors across the
//!   whole window; the driver relies on this, writing TEMP/SCRATCH through
//!   the `+$40` shadow (a 68030/68040 cache write-allocate workaround) and
//!   reading them back at the base offsets. Registers are plain storage for
//!   now -- enough for the driver's walking-bits hardware test -- with the
//!   SCRIPTS processor still to come (a DSP write warns once).
//! - `$8C0003` the DIP-switch byte (SCSI host ID, termination, sync/fast
//!   negotiation enables). `$FF` means all switches off: host ID 7.
//!
//! The autoconfig identity (Commodore, product 84, 16M Zorro III,
//! er_InitDiagVec `$0200`) is supplied by [`crate::zorro::BoardSpec::a4091`];
//! the same nibbles are also baked into the first `$60` bytes of the real
//! EPROM, which is how the physical board presents them.

use crate::scsi::{ScsiDisk, ScsiExec};
use anyhow::{bail, Result};
use std::collections::VecDeque;
use std::path::Path;

/// The 53C710 register window within the board space.
const IO_OFFSET: u32 = 0x0080_0000;
const IO_END: u32 = 0x0088_0000;

/// The DIP-switch readback byte.
const DIP_OFFSET: u32 = 0x008C_0003;

/// 53C710 register-file byte offsets as the 68k sees them (the chip is
/// wired big-endian on the A4091, so these are the driver's REG_ addresses).
const REG_CTEST8: usize = 0x21;
/// CTEST8.CLF: clear the DMA and SCSI FIFOs; self-clearing strobe.
const CTEST8_CLF: u8 = 0x04;
/// ISTAT: bit 7 abort strobe, bit 6 software reset; bits 1/0 are the
/// computed SIP/DIP interrupt-pending flags.
const REG_ISTAT: usize = 0x22;
const ISTAT_ABRT: u8 = 0x80;
const ISTAT_RST: u8 = 0x40;
const ISTAT_SIP: u8 = 0x02;
const ISTAT_DIP: u8 = 0x01;
/// DIEN: enable mask for DSTAT interrupt causes.
const REG_DIEN: usize = 0x3A;
/// DSTAT bit 4: aborted.
const DSTAT_ABRT: u8 = 0x10;
/// SIEN: enable mask for SSTAT0 interrupt causes.
const REG_SIEN: usize = 0x00;
/// SSTAT0: SCSI interrupt causes (read-clear).
const REG_SSTAT0: usize = 0x0E;
/// DSTAT: bit 7 (DFE) tracks "all DMA FIFO lanes empty".
const REG_DSTAT: usize = 0x0F;
const DSTAT_DFE: u8 = 0x80;
/// CTEST1: FMT lane-empty flags in the high nibble, FFL lane-full in the low.
const REG_CTEST1: usize = 0x16;
/// CTEST2: bit 3 carries the parity bit of the last DMA FIFO pop.
const REG_CTEST2: usize = 0x15;
const CTEST2_DFP: u8 = 0x08;
/// CTEST4: FBL2 (bit 2) routes CTEST6 to the DMA FIFO lane in bits 1:0.
const REG_CTEST4: usize = 0x1B;
const CTEST4_FBL2: u8 = 0x04;
/// CTEST6: the DMA FIFO data window (with FBL2 set).
const REG_CTEST6: usize = 0x19;
/// CTEST7: bit 3 supplies the parity bit pushed with a DMA FIFO write.
const REG_CTEST7: usize = 0x18;
const CTEST7_DFP: u8 = 0x08;
/// CTEST5 carries the ADCK/BBCK self-clearing test strobes.
const REG_CTEST5: usize = 0x1A;
const CTEST5_ADCK: u8 = 0x80;
const CTEST5_BBCK: u8 = 0x40;
/// DNAD: 32-bit DMA next address; ADCK increments it by the bus width.
const REG_DNAD: usize = 0x28;
/// DBC: 24-bit DMA byte counter (below DCMD at $24); BBCK decrements it.
const REG_DBC: usize = 0x25;
/// DSP: writing its low byte starts the SCRIPTS processor.
const REG_DSP: usize = 0x2C;
/// DSPS: the operand of the last INT instruction.
const REG_DSPS: usize = 0x30;
/// TEMP: the SCRIPTS return-address register.
const REG_TEMP: usize = 0x1C;
/// DSTAT causes the SCRIPTS engine raises.
const DSTAT_SIR: u8 = 0x04;
const DSTAT_IID: u8 = 0x01;
/// DMA FIFO depth per byte lane.
const DMA_FIFO_DEPTH: usize = 16;
/// SCNTL0: bit 0 target mode, bit 2 enable parity generation.
const REG_SCNTL0: usize = 0x03;
const SCNTL0_TRG: u8 = 0x01;
const SCNTL0_EPG: u8 = 0x04;
/// SCNTL1: bit 2 asserts even (instead of odd) generated parity; bit 3
/// asserts a SCSI bus reset; bit 6 asserts the data bus, driving SODL/SOCL
/// onto the (looped-back) SCSI lines.
const REG_SCNTL1: usize = 0x02;
const SCNTL1_AESP: u8 = 0x04;
const SCNTL1_RST: u8 = 0x08;
const SCNTL1_ADB: u8 = 0x40;
/// SOCL: SCSI output control latch, driven onto the control lines with ADB.
const REG_SOCL: usize = 0x04;
/// SODL: writes push the SCSI FIFO when CTEST4.SFWR routes them there.
const REG_SODL: usize = 0x05;
/// SBCL: SCSI bus control lines (input); mirrors SOCL in loopback.
const REG_SBCL: usize = 0x08;
/// SBDL: SCSI bus data lines (input); mirrors SODL in loopback.
const REG_SBDL: usize = 0x09;
/// SSTAT1: bit 0 the generated data parity, bit 1 SCSI bus reset asserted.
const REG_SSTAT1: usize = 0x0D;
const SSTAT1_PAR: u8 = 0x01;
const SSTAT1_RST: u8 = 0x02;
/// CTEST4 bit 4: SCSI loopback -- the output latches feed the input registers.
const CTEST4_SLBE: u8 = 0x10;
/// SBCL/SOCL bit 3 (ACK) and bit 6 (REQ): assert only in target mode.
const SBCL_TARGET_ONLY: u8 = 0x48;
/// SSTAT2: SCSI FIFO fill count in the high nibble.
const REG_SSTAT2: usize = 0x0C;
/// CTEST3: reads pop the SCSI FIFO.
const REG_CTEST3: usize = 0x14;
/// CTEST4 bit 3: route SODL writes to the SCSI FIFO.
const CTEST4_SFWR: u8 = 0x08;
/// CTEST2 bit 4: parity bit of the last SCSI FIFO pop.
const CTEST2_SFP: u8 = 0x10;
/// SCSI FIFO depth.
const SCSI_FIFO_DEPTH: usize = 8;

/// SDID: SCSI Destination ID the SELECT resolved to.
const REG_SDID: usize = 0x01;
/// SFBR: SCSI First Byte Received -- the last byte the SCRIPTS moved, which
/// the transfer-control data compares (JUMP IF 0xNN) test against.
const REG_SFBR: usize = 0x0B;
/// DSA: the 32-bit Data Structure Address that table-indirect operands (the
/// block-move tables and the SELECT target) are relative to.
const REG_DSA: usize = 0x10;
/// SSTAT0 SCSI-interrupt causes the phase engine raises.
const SSTAT0_STO: u8 = 0x20; // (re)selection timeout -- no such target
/// Byte counts of the SCRIPTS data structure entries live in the low 24 bits.
const DBC_MASK: u32 = 0x00FF_FFFF;

/// SCSI bus information-transfer phases, encoded as the MSG/C_D/I_O signal
/// triple the 53C710 phase compares match against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    DataOut,
    DataIn,
    Command,
    Status,
    MsgOut,
    MsgIn,
}

impl Phase {
    fn bits(self) -> u8 {
        match self {
            Phase::DataOut => 0,
            Phase::DataIn => 1,
            Phase::Command => 2,
            Phase::Status => 3,
            Phase::MsgOut => 6,
            Phase::MsgIn => 7,
        }
    }
}

/// A live SCSI connection (nexus) driven by the SCRIPTS phase engine. The
/// target sequences MSG_OUT (accept IDENTIFY) -> COMMAND (accept the CDB) ->
/// DATA_IN/OUT -> STATUS -> MSG_IN (command complete) -> disconnect; each
/// block move transfers one phase's worth and advances the state.
struct Connection {
    target: usize,
    lun: u8,
    phase: Phase,
    /// Bytes the target still owes the initiator in the current in-phase
    /// (DATA_IN / STATUS / MSG_IN).
    out: VecDeque<u8>,
    /// The command block collected during COMMAND phase.
    cdb: Vec<u8>,
    /// DATA_OUT sink: bytes collected and the count still expected.
    data_out: Vec<u8>,
    data_out_needed: usize,
    /// Ending status byte, delivered in STATUS phase.
    status: u8,
    /// After the current MSG_IN drains, the phase to resume in (COMMAND after
    /// a rejected sync message; MSG_IN keeps disconnect_ready for completion).
    after_msg_in: Phase,
    /// The command-complete message has been delivered; WAIT DISCONNECT frees
    /// the bus.
    disconnect_ready: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct A4091 {
    rom: Vec<u8>,
    /// DIP switches as read at `$8C0003`; `$FF` = all off (host ID 7).
    dip: u8,
    /// The 53C710 register file, indexed by CPU-visible (big-endian) byte
    /// address. Plain storage until the SCRIPTS core lands.
    regs: Vec<u8>,
    /// The DMA FIFO: four byte lanes, 16 entries deep, 8 data bits plus a
    /// parity bit per entry. CTEST6 pushes/pops the lane selected by
    /// CTEST4; CTEST1 and DSTAT.DFE report the fill state.
    dma_fifo: [Vec<u16>; 4],
    /// The SCSI FIFO: 8 entries, 8 data bits plus parity. SODL pushes it
    /// (with CTEST4.SFWR), CTEST3 pops it, SSTAT2 counts it.
    scsi_fifo: Vec<u16>,
    /// SCSI-2 disk targets on the bus, indexed by SCSI ID (0-6; 7 is the
    /// host adapter). Kept across resets; part of save state.
    #[serde(default)]
    targets: [Option<ScsiDisk>; 7],
    /// The live nexus while a SCRIPTS command is in flight. Rebuilt as the
    /// driver reissues commands, so it is not persisted.
    #[serde(skip)]
    conn: Option<Connection>,
}

/// 53C710 register-file reset values at their big-endian byte addresses:
/// SCNTL0 (arbitration bits), SCID, DSTAT.DFE (DMA FIFO empty), CTEST2.DACK,
/// DCMD, matching the chip's documented power-on state.
fn reset_regs() -> Vec<u8> {
    let mut r = vec![0u8; 0x40];
    r[0x03] = 0xC0; // SCNTL0
    r[0x07] = 0x80; // SCID
                    // DSTAT.DFE and CTEST1 are computed from the DMA FIFO state on read.
    r[0x15] = 0x01; // CTEST2: DACK
    r[0x24] = 0x40; // DCMD
    r
}

impl A4091 {
    /// Build the board from its boot ROM image (a raw 32K or 64K byte-wide
    /// EPROM dump; the board serves it nibble-wide).
    pub fn new(rom: Vec<u8>) -> Result<Self> {
        if !matches!(rom.len(), 0x8000 | 0x1_0000) {
            bail!(
                "A4091 ROM is {} bytes; expected 32K or 64K (a raw byte-wide \
                 EPROM image, e.g. the open-source a4091.rom)",
                rom.len()
            );
        }
        Ok(Self {
            rom,
            dip: 0xFF,
            regs: reset_regs(),
            dma_fifo: Default::default(),
            scsi_fifo: Vec::new(),
            targets: Default::default(),
            conn: None,
        })
    }

    /// Attach a disk target at the given SCSI ID (0-6).
    pub fn attach_drive(&mut self, id: usize, disk: ScsiDisk) {
        if id < self.targets.len() {
            self.targets[id] = Some(disk);
        }
    }

    /// Whether a target answers at the given SCSI ID.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn target_present(&self, id: usize) -> bool {
        self.targets.get(id).is_some_and(Option::is_some)
    }

    /// The parity bit the chip generates for a pushed SCSI FIFO byte: odd
    /// parity normally, even when SCNTL1.AESP is set. (SCNTL0.EPG gates
    /// generation on the real chip; without it parity would come from the
    /// bus, which has no meaning here, so the generated bit is used
    /// regardless.)
    fn scsi_parity(&self, b: u8) -> u16 {
        let even = u16::from(b.count_ones() as u8 & 1);
        if self.regs[REG_SCNTL1] & SCNTL1_AESP != 0 {
            even
        } else {
            even ^ 1
        }
    }

    /// In loopback (CTEST4.SLBE) with the data bus asserted (SCNTL1.ADB),
    /// the SODL/SOCL output latches feed straight back into the SBDL/SBCL
    /// input registers -- there is no real bus. The SCSI pin self-test walks
    /// patterns through this path.
    fn loopback(&self) -> bool {
        self.regs[REG_CTEST4] & CTEST4_SLBE != 0 && self.regs[REG_SCNTL1] & SCNTL1_ADB != 0
    }

    /// The DMA FIFO lane CTEST6 currently addresses, when CTEST4.FBL2
    /// routes it to the FIFO at all.
    fn fifo_lane(&self) -> Option<usize> {
        (self.regs[REG_CTEST4] & CTEST4_FBL2 != 0).then_some((self.regs[REG_CTEST4] & 3) as usize)
    }

    /// CTEST1 computed from the FIFO: FMT empty flags high, FFL full low.
    fn ctest1(&self) -> u8 {
        let mut v = 0u8;
        for (lane, fifo) in self.dma_fifo.iter().enumerate() {
            if fifo.is_empty() {
                v |= 0x10 << lane;
            }
            if fifo.len() >= DMA_FIFO_DEPTH {
                v |= 1 << lane;
            }
        }
        v
    }

    /// Whether the chip's interrupt output is asserted: any enabled DMA
    /// cause (DSTAT masked by DIEN) or SCSI cause (SSTAT0 masked by SIEN).
    /// DSTAT.DFE is status, not a cause, and never contributes.
    fn irq_line(&self) -> bool {
        (self.regs[REG_DSTAT] & self.regs[REG_DIEN] & !DSTAT_DFE) != 0
            || (self.regs[REG_SSTAT0] & self.regs[REG_SIEN]) != 0
    }

    /// Software reset (power-on, /RST, or the ISTAT RST strobe): registers
    /// to documented reset values, FIFOs drained. ROM and switches keep.
    fn chip_reset(&mut self) {
        self.regs = reset_regs();
        for fifo in &mut self.dma_fifo {
            fifo.clear();
        }
        self.scsi_fifo.clear();
        self.conn = None;
    }

    pub fn load_rom(path: &Path) -> Result<Vec<u8>> {
        std::fs::read(path)
            .map_err(|e| anyhow::anyhow!("reading A4091 ROM {}: {e}", path.display()))
    }

    fn reg32(&self, r: usize) -> u32 {
        u32::from_be_bytes(self.regs[r..r + 4].try_into().unwrap())
    }

    fn set_reg32(&mut self, r: usize, v: u32) {
        self.regs[r..r + 4].copy_from_slice(&v.to_be_bytes());
    }

    /// CTEST5 ADCK/BBCK test strobes: increment DNAD / decrement DBC by the
    /// bus width, then self-clear.
    fn ctest5_strobes(&mut self) {
        let v = self.regs[REG_CTEST5];
        if v & CTEST5_ADCK != 0 {
            let dnad = self.reg32(REG_DNAD).wrapping_add(4);
            self.set_reg32(REG_DNAD, dnad);
        }
        if v & CTEST5_BBCK != 0 {
            let dbc = (u32::from(self.regs[REG_DBC]) << 16)
                | (u32::from(self.regs[REG_DBC + 1]) << 8)
                | u32::from(self.regs[REG_DBC + 2]);
            let dbc = dbc.wrapping_sub(4) & 0x00FF_FFFF;
            self.regs[REG_DBC] = (dbc >> 16) as u8;
            self.regs[REG_DBC + 1] = (dbc >> 8) as u8;
            self.regs[REG_DBC + 2] = dbc as u8;
        }
        self.regs[REG_CTEST5] = v & !(CTEST5_ADCK | CTEST5_BBCK);
    }

    /// Map a chip-native (little-endian) register number to its big-endian
    /// byte address in `regs`. SCRIPTS register operands use the native
    /// numbering; the 68k byte lanes are swapped within each longword.
    fn le_to_be(le: usize) -> usize {
        (le & !3) | (3 - (le & 3))
    }

    /// One byte as the SCRIPTS DMA engine sees the bus: RAM through the
    /// host decode, the board's own window (register file / ROM / DIP)
    /// directly -- the self-test DMAs the chip's own SCRATCH and TEMP.
    /// Register bytes are accessed as plain storage, without read side
    /// effects; the self-tests only move SCRATCH/TEMP this way.
    fn dma_byte(&self, host: &crate::zorro_device::DeviceHost, addr: u32) -> u8 {
        match host.own_window_offset(addr) {
            Some(off) if (IO_OFFSET..IO_END).contains(&off) => self.regs[(off as usize) & 0x3F],
            Some(off) if off < IO_OFFSET => self.rom_byte(off),
            Some(off) if off == DIP_OFFSET => self.dip,
            Some(_) => 0xFF,
            None => host.dma_read_byte(addr).unwrap_or(0xFF),
        }
    }

    fn dma_write_byte(&mut self, host: &mut crate::zorro_device::DeviceHost, addr: u32, b: u8) {
        match host.own_window_offset(addr) {
            Some(off) if (IO_OFFSET..IO_END).contains(&off) => {
                self.regs[(off as usize) & 0x3F] = b;
            }
            Some(_) => {} // ROM and switches are not writable
            None => {
                host.dma_write(addr, &[b]);
            }
        }
    }

    /// A big-endian longword read from Amiga RAM (a SCRIPTS table entry).
    fn dma_read32(&self, host: &mut crate::zorro_device::DeviceHost, addr: u32) -> u32 {
        let mut b = [0u8; 4];
        host.dma_read(addr, &mut b);
        u32::from_be_bytes(b)
    }

    /// The SCSI phase the connected target is presenting, for the SCRIPTS
    /// phase compares. `0xFF` when the bus is free (matches no phase).
    fn phase_bits(&self) -> u8 {
        self.conn.as_ref().map_or(0xFF, |c| c.phase.bits())
    }

    /// Begin a SELECT: resolve the target from the `ds_Device` table entry at
    /// DSA+offset (a one-hot ID bitmask in bits 16-23). A present target opens
    /// a nexus in MSG_OUT (to accept the IDENTIFY); an empty ID raises a
    /// selection timeout, as the driver expects when probing for devices.
    fn scsi_select(&mut self, host: &mut crate::zorro_device::DeviceHost, offset: u32) {
        let dsa = self.reg32(REG_DSA);
        let entry = self.dma_read32(host, dsa.wrapping_add(offset));
        let id_bits = (entry >> 16) & 0xFF;
        let target = id_bits.trailing_zeros() as usize;
        if id_bits == 0 || !self.target_present(target) {
            self.regs[REG_SSTAT0] |= SSTAT0_STO;
            self.conn = None;
            return;
        }
        self.regs[REG_SDID] = target as u8;
        self.conn = Some(Connection {
            target,
            lun: 0,
            phase: Phase::MsgOut,
            out: VecDeque::new(),
            cdb: Vec::new(),
            data_out: Vec::new(),
            data_out_needed: 0,
            status: 0,
            after_msg_in: Phase::Command,
            disconnect_ready: false,
        });
    }

    /// Table-indirect block move for the current phase: transfer up to the
    /// entry's byte count between the target and the initiator buffer at
    /// DSA+offset, advancing the target's phase as each phase drains.
    fn run_block_move(&mut self, host: &mut crate::zorro_device::DeviceHost, offset: u32) {
        let dsa = self.reg32(REG_DSA);
        let count = (self.dma_read32(host, dsa.wrapping_add(offset)) & DBC_MASK) as usize;
        let addr = self.dma_read32(host, dsa.wrapping_add(offset).wrapping_add(4));
        let Some(phase) = self.conn.as_ref().map(|c| c.phase) else {
            return;
        };
        match phase {
            // Initiator -> target directions.
            Phase::MsgOut => {
                let mut msg = vec![0u8; count];
                host.dma_read(addr, &mut msg);
                if let Some(&b) = msg.last() {
                    self.regs[REG_SFBR] = b;
                }
                self.scsi_msg_out(&msg);
            }
            Phase::Command => {
                let mut cdb = vec![0u8; count];
                host.dma_read(addr, &mut cdb);
                if let Some(&b) = cdb.last() {
                    self.regs[REG_SFBR] = b;
                }
                self.scsi_command(&cdb);
            }
            Phase::DataOut => {
                let mut buf = vec![0u8; count];
                host.dma_read(addr, &mut buf);
                if let Some(&b) = buf.last() {
                    self.regs[REG_SFBR] = b;
                }
                if let Some(c) = self.conn.as_mut() {
                    c.data_out.extend_from_slice(&buf);
                }
                let done = self
                    .conn
                    .as_ref()
                    .is_some_and(|c| c.data_out.len() >= c.data_out_needed);
                if done {
                    self.scsi_data_out_done();
                }
            }
            // Target -> initiator directions.
            Phase::DataIn | Phase::Status | Phase::MsgIn => {
                let bytes: Vec<u8> = {
                    let c = self.conn.as_mut().unwrap();
                    let n = count.min(c.out.len());
                    c.out.drain(..n).collect()
                };
                if let Some(&b) = bytes.last() {
                    self.regs[REG_SFBR] = b;
                }
                host.dma_write(addr, &bytes);
                if self.conn.as_ref().is_some_and(|c| c.out.is_empty()) {
                    self.advance_in_phase();
                }
            }
        }
    }

    /// Consume the MSG_OUT bytes (IDENTIFY plus any negotiation message). A
    /// following extended message (a sync/wide request we do not support) is
    /// answered with MESSAGE REJECT; otherwise proceed to COMMAND.
    fn scsi_msg_out(&mut self, msg: &[u8]) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        if let Some(&id) = msg.first() {
            if id & 0x80 != 0 {
                conn.lun = id & 0x07;
            }
        }
        if msg.len() > 1 && msg.get(1) == Some(&0x01) {
            conn.out = VecDeque::from(vec![0x07]); // MESSAGE REJECT
            conn.after_msg_in = Phase::Command;
            conn.phase = Phase::MsgIn;
        } else {
            conn.phase = Phase::Command;
        }
    }

    /// Run the collected CDB against the target and set up the data phase.
    fn scsi_command(&mut self, cdb: &[u8]) {
        let Some((target, lun)) = self.conn.as_ref().map(|c| (c.target, c.lun)) else {
            return;
        };
        if let Some(c) = self.conn.as_mut() {
            c.cdb = cdb.to_vec();
        }
        let (exec, status) = match self.targets[target].as_mut() {
            Some(disk) => disk.execute(cdb, lun),
            None => (ScsiExec::NoData, 0x02),
        };
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        conn.status = status;
        match exec {
            ScsiExec::DataIn(data) if !data.is_empty() => {
                conn.out = VecDeque::from(data);
                conn.phase = Phase::DataIn;
            }
            ScsiExec::DataOut(n) => {
                conn.data_out_needed = n;
                conn.data_out.clear();
                conn.phase = Phase::DataOut;
            }
            // No data, or an empty data-in: straight to status.
            _ => {
                conn.out = VecDeque::from(vec![status]);
                conn.phase = Phase::Status;
            }
        }
    }

    /// The DATA_OUT payload is complete: hand it to the target and move to
    /// status.
    fn scsi_data_out_done(&mut self) {
        let Some((target, cdb, data)) = self
            .conn
            .as_ref()
            .map(|c| (c.target, c.cdb.clone(), c.data_out.clone()))
        else {
            return;
        };
        let status = match self.targets[target].as_mut() {
            Some(disk) => disk.complete_out(&cdb, &data),
            None => 0x02,
        };
        if let Some(conn) = self.conn.as_mut() {
            conn.status = status;
            conn.out = VecDeque::from(vec![status]);
            conn.phase = Phase::Status;
        }
    }

    /// A target->initiator phase drained: advance to the next. DATA_IN is
    /// followed by STATUS, STATUS by the command-complete MSG_IN, and the
    /// MESSAGE REJECT MSG_IN resumes COMMAND.
    fn advance_in_phase(&mut self) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        match conn.phase {
            Phase::DataIn => {
                conn.out = VecDeque::from(vec![conn.status]);
                conn.phase = Phase::Status;
            }
            Phase::Status => {
                conn.out = VecDeque::from(vec![0x00]); // command complete
                conn.phase = Phase::MsgIn;
                conn.disconnect_ready = true;
            }
            Phase::MsgIn if !conn.disconnect_ready => {
                conn.phase = conn.after_msg_in;
            }
            // A completed command's MSG_IN stays until WAIT DISCONNECT.
            _ => {}
        }
    }

    /// Evaluate a transfer-control condition (phase and/or data compare
    /// against SFBR), returning whether the branch is taken.
    fn branch_taken(&self, insn: &[u8; 8]) -> bool {
        let dcmd = insn[0];
        let flags = insn[1];
        let want_true = flags & 0x08 != 0;
        let mut cond = true;
        if flags & 0x02 != 0 {
            cond &= self.phase_bits() == (dcmd & 7);
        }
        if flags & 0x04 != 0 {
            let mask = insn[2];
            let data = insn[3];
            cond &= (self.regs[REG_SFBR] & !mask) == (data & !mask);
        }
        cond == want_true
    }

    /// Run the SCRIPTS processor from DSP until it interrupts, disconnects, or
    /// hits an instruction it does not implement. Covers the driver's I/O
    /// script: SELECT, table-indirect block moves through the SCSI phases,
    /// conditional JUMP/CALL/RETURN/INT, the SET/CLEAR handshake ops, and
    /// WAIT DISCONNECT, plus the self-test's register writes and Memory Move.
    fn run_scripts(&mut self, host: &mut crate::zorro_device::DeviceHost) {
        let diag = crate::envcfg::flag("COPPERLINE_DIAG_A4091");
        for _ in 0..4096 {
            let dsp = self.reg32(REG_DSP);
            let mut insn = [0u8; 8];
            host.dma_read(dsp, &mut insn);
            self.set_reg32(REG_DSP, dsp.wrapping_add(8));
            let dcmd = insn[0];
            if diag {
                log::info!(
                    "a4091 scripts: fetch {dsp:#010X}: {insn:02X?} phase={:02X}",
                    self.phase_bits()
                );
            }
            match dcmd >> 6 {
                // Block move: table-indirect transfer in the current phase.
                0b00 => {
                    let offset = u32::from_be_bytes(insn[4..8].try_into().unwrap());
                    self.run_block_move(host, offset);
                }
                // I/O instructions.
                0b01 => match (dcmd >> 3) & 7 {
                    // SELECT: target from the table entry at DSA+offset.
                    0 => {
                        let offset = u32::from_be_bytes(insn[0..4].try_into().unwrap()) & DBC_MASK;
                        self.scsi_select(host, offset);
                        if self.conn.is_none() {
                            return; // selection timeout: let the ISR handle it
                        }
                    }
                    // WAIT DISCONNECT: free the bus after the target completes.
                    1 => {
                        if self.conn.as_ref().is_some_and(|c| c.disconnect_ready) {
                            self.conn = None;
                        }
                    }
                    // SET / CLEAR ATN/ACK: handshake, no byte-level model.
                    3 | 4 => {}
                    // MOVE immediate to register (self-test path).
                    7 if dcmd == 0x78 => {
                        let reg = Self::le_to_be((insn[1] & 0x3F) as usize);
                        self.regs[reg] = insn[2];
                    }
                    // WAIT RESELECT and register-to-register moves live only on
                    // the reconnect path, which the synchronous model never
                    // reaches.
                    _ => {
                        self.regs[REG_DSTAT] |= DSTAT_IID;
                        return;
                    }
                },
                // Transfer control: JUMP / CALL / RETURN / INT, conditional.
                0b10 => {
                    let taken = self.branch_taken(&insn);
                    let op = (dcmd >> 3) & 7;
                    if op == 3 {
                        // INT: interrupt the host with the operand in DSPS.
                        if taken {
                            let dsps = u32::from_be_bytes(insn[4..8].try_into().unwrap());
                            self.set_reg32(REG_DSPS, dsps);
                            self.regs[REG_DSTAT] |= DSTAT_SIR;
                            return;
                        }
                    } else if taken {
                        let operand = u32::from_be_bytes(insn[4..8].try_into().unwrap());
                        let target = if insn[1] & 0x80 != 0 {
                            dsp.wrapping_add(8).wrapping_add(operand) // relative
                        } else {
                            operand
                        };
                        match op {
                            0 => self.set_reg32(REG_DSP, target), // JUMP
                            1 => {
                                // CALL: stack the return address in TEMP.
                                self.set_reg32(REG_TEMP, dsp.wrapping_add(8));
                                self.set_reg32(REG_DSP, target);
                            }
                            2 => {
                                let temp = self.reg32(REG_TEMP); // RETURN
                                self.set_reg32(REG_DSP, temp);
                            }
                            _ => {
                                self.regs[REG_DSTAT] |= DSTAT_IID;
                                return;
                            }
                        }
                    }
                }
                // Memory Move: 24-bit byte count, then source and
                // destination addresses in a third fetched longword.
                0b11 if dcmd & 0x38 == 0 => {
                    let mut dst = [0u8; 4];
                    host.dma_read(dsp.wrapping_add(8), &mut dst);
                    self.set_reg32(REG_DSP, dsp.wrapping_add(12));
                    let count = u32::from_be_bytes(insn[0..4].try_into().unwrap()) & 0x00FF_FFFF;
                    let src = u32::from_be_bytes(insn[4..8].try_into().unwrap());
                    let dst = u32::from_be_bytes(dst);
                    for i in 0..count {
                        let b = self.dma_byte(host, src.wrapping_add(i));
                        self.dma_write_byte(host, dst.wrapping_add(i), b);
                        if diag && i < 8 {
                            log::info!(
                                "a4091 scripts: move [{:#010X}] {b:02X} -> [{:#010X}]",
                                src.wrapping_add(i),
                                dst.wrapping_add(i)
                            );
                        }
                    }
                    // The byte counter runs down to zero.
                    self.regs[REG_DBC] = 0;
                    self.regs[REG_DBC + 1] = 0;
                    self.regs[REG_DBC + 2] = 0;
                }
                _ => {
                    self.regs[REG_DSTAT] |= DSTAT_IID;
                    return;
                }
            }
        }
        // Runaway script: stop as illegal.
        self.regs[REG_DSTAT] |= DSTAT_IID;
    }

    /// One byte of the nibble-wide ROM as seen in the window at `off`.
    fn rom_byte(&self, off: u32) -> u8 {
        if off & 1 != 0 {
            return 0xFF;
        }
        let b = self.rom[(off as usize / 4) % self.rom.len()];
        if off & 2 == 0 {
            b | 0x0F
        } else {
            (b << 4) | 0x0F
        }
    }
}

impl crate::zorro_device::ZorroDevice for A4091 {
    fn read(&mut self, off: u32, size: usize, _host: &mut crate::zorro_device::DeviceHost) -> u32 {
        let mut v = 0u32;
        for i in 0..size {
            let o = off.wrapping_add(i as u32);
            let b = if o < IO_OFFSET {
                self.rom_byte(o)
            } else if o == DIP_OFFSET {
                self.dip
            } else if (IO_OFFSET..IO_END).contains(&o) {
                match (o as usize) & 0x3F {
                    // CTEST8: chip revision 2 in the high nibble; the CLF
                    // (clear FIFOs) strobe bit always reads back 0.
                    REG_CTEST8 => (self.regs[REG_CTEST8] | 0x20) & !0x04,
                    REG_CTEST1 => self.ctest1(),
                    // DSTAT: cause bits clear on read; DFE is live status.
                    REG_DSTAT => {
                        let dfe = self.dma_fifo.iter().all(Vec::is_empty);
                        let v =
                            (self.regs[REG_DSTAT] & !DSTAT_DFE) | if dfe { DSTAT_DFE } else { 0 };
                        self.regs[REG_DSTAT] = 0;
                        v
                    }
                    // SSTAT0: cause bits clear on read.
                    REG_SSTAT0 => {
                        let v = self.regs[REG_SSTAT0];
                        self.regs[REG_SSTAT0] = 0;
                        v
                    }
                    // ISTAT: stored strobe bits plus computed DIP/SIP.
                    REG_ISTAT => {
                        let dip = self.regs[REG_DSTAT] & !DSTAT_DFE != 0;
                        let sip = self.regs[REG_SSTAT0] != 0;
                        (self.regs[REG_ISTAT] & 0xF0)
                            | if sip { ISTAT_SIP } else { 0 }
                            | if dip { ISTAT_DIP } else { 0 }
                    }
                    REG_SSTAT2 => {
                        ((self.scsi_fifo.len() as u8) << 4) | (self.regs[REG_SSTAT2] & 0x0F)
                    }
                    // SBDL mirrors the driven data latch in loopback; the
                    // idle bus reads all-deasserted (0).
                    REG_SBDL => {
                        if self.loopback() {
                            self.regs[REG_SODL]
                        } else {
                            0
                        }
                    }
                    // SBCL mirrors the driven control latch in loopback;
                    // ACK/REQ (bits 3/6) assert only in target mode.
                    REG_SBCL => {
                        if self.loopback() {
                            let c = self.regs[REG_SOCL];
                            if self.regs[REG_SCNTL0] & SCNTL0_TRG != 0 {
                                c
                            } else {
                                c & !SBCL_TARGET_ONLY
                            }
                        } else {
                            0
                        }
                    }
                    // SSTAT1: RST reflects the SCNTL1.RST bus-reset request;
                    // PAR is the generated parity of the driven data.
                    REG_SSTAT1 => {
                        let mut v = self.regs[REG_SSTAT1] & !(SSTAT1_RST | SSTAT1_PAR);
                        if self.regs[REG_SCNTL1] & SCNTL1_RST != 0 {
                            v |= SSTAT1_RST;
                        }
                        if self.loopback()
                            && self.regs[REG_SCNTL0] & SCNTL0_EPG != 0
                            && self.scsi_parity(self.regs[REG_SODL]) & 1 != 0
                        {
                            v |= SSTAT1_PAR;
                        }
                        v
                    }
                    // CTEST3 pops the SCSI FIFO; parity lands in CTEST2.SFP.
                    REG_CTEST3 if !self.scsi_fifo.is_empty() => {
                        let entry = self.scsi_fifo.remove(0);
                        self.regs[REG_CTEST2] = (self.regs[REG_CTEST2] & !CTEST2_SFP)
                            | if entry & 0x100 != 0 { CTEST2_SFP } else { 0 };
                        entry as u8
                    }
                    // CTEST6 pops the addressed DMA FIFO lane; the entry's
                    // parity bit lands in CTEST2.DFP.
                    REG_CTEST6 => match self.fifo_lane() {
                        Some(lane) if !self.dma_fifo[lane].is_empty() => {
                            let entry = self.dma_fifo[lane].remove(0);
                            self.regs[REG_CTEST2] = (self.regs[REG_CTEST2] & !CTEST2_DFP)
                                | if entry & 0x100 != 0 { CTEST2_DFP } else { 0 };
                            entry as u8
                        }
                        _ => self.regs[REG_CTEST6],
                    },
                    r => self.regs[r],
                }
            } else {
                0xFF
            };
            v = (v << 8) | u32::from(b);
        }
        v
    }

    fn write(
        &mut self,
        off: u32,
        size: usize,
        value: u32,
        host: &mut crate::zorro_device::DeviceHost,
    ) {
        for i in 0..size {
            let o = off.wrapping_add(i as u32);
            if !(IO_OFFSET..IO_END).contains(&o) {
                continue;
            }
            let r = (o as usize) & 0x3F;
            let b = (value >> (8 * (size - 1 - i))) as u8;
            // SODL with SFWR set pushes the SCSI FIFO as well as storing.
            if r == REG_SODL
                && self.regs[REG_CTEST4] & CTEST4_SFWR != 0
                && self.scsi_fifo.len() < SCSI_FIFO_DEPTH
            {
                let parity = self.scsi_parity(b);
                self.scsi_fifo.push((parity << 8) | u16::from(b));
            }
            // CTEST6 with the FIFO addressed pushes instead of storing.
            if r == REG_CTEST6 {
                if let Some(lane) = self.fifo_lane() {
                    if self.dma_fifo[lane].len() < DMA_FIFO_DEPTH {
                        let parity = u16::from(self.regs[REG_CTEST7] & CTEST7_DFP != 0);
                        self.dma_fifo[lane].push((parity << 8) | u16::from(b));
                    }
                    continue;
                }
            }
            if r == REG_ISTAT {
                // DIP/SIP (low nibble) are computed status, not writable.
                self.regs[REG_ISTAT] = b & 0xF0;
                if b & ISTAT_RST != 0 {
                    self.chip_reset();
                    self.regs[REG_ISTAT] = ISTAT_RST;
                }
                // Abort: latch the DSTAT cause; DIEN gates only the line.
                if b & ISTAT_ABRT != 0 {
                    self.regs[REG_DSTAT] |= DSTAT_ABRT;
                }
                continue;
            }
            self.regs[r] = b;
            if r == REG_CTEST5 {
                self.ctest5_strobes();
            }
            // CTEST8.CLF drains both FIFOs; the bit does not latch.
            if r == REG_CTEST8 && b & CTEST8_CLF != 0 {
                for fifo in &mut self.dma_fifo {
                    fifo.clear();
                }
                self.scsi_fifo.clear();
                self.regs[REG_CTEST8] = b & !CTEST8_CLF;
            }
            // Writing DSP's low byte starts the SCRIPTS processor.
            if r == REG_DSP + 3 {
                self.run_scripts(host);
            }
        }
    }

    fn peek_word(&self, off: u32) -> Option<u16> {
        if off < IO_OFFSET {
            Some(
                (u16::from(self.rom_byte(off)) << 8)
                    | u16::from(self.rom_byte(off.wrapping_add(1))),
            )
        } else {
            None
        }
    }

    fn tick(&mut self, _cck: u32, _host: &mut crate::zorro_device::DeviceHost) {}

    fn int2_line(&self) -> bool {
        self.irq_line()
    }

    fn reset(&mut self) {
        self.chip_reset();
    }

    fn kind(&self) -> &'static str {
        "a4091"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Memory;
    use crate::zorro::ZorroChain;
    use crate::zorro_device::{DeviceHost, ZorroDevice};

    fn board_with_rom(rom: Vec<u8>) -> A4091 {
        A4091::new(rom).expect("valid ROM size")
    }

    fn test_memory() -> Memory {
        Memory {
            chip_ram: vec![0u8; 512 * 1024],
            slow_ram: Vec::new(),
            rom: Vec::new(),
            overlay: false,
            zorro: ZorroChain::default(),
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs: Vec::new(),
            wcs_write_protected: false,
        }
    }

    fn test_rom_64k() -> Vec<u8> {
        (0..0x1_0000usize).map(|i| i as u8).collect()
    }

    #[test]
    fn rom_appears_nibble_wide() {
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        // ROM byte $12 lives at window offset $48 (high nibble) / $4A (low).
        assert_eq!(b.read(0x48, 1, &mut host), 0x1F);
        assert_eq!(b.read(0x4A, 1, &mut host), 0x2F);
        assert_eq!(b.read(0x49, 1, &mut host), 0xFF);
        assert_eq!(b.read(0x4B, 1, &mut host), 0xFF);
    }

    #[test]
    fn rom_mirrors_across_the_rom_space_and_32k_images_repeat() {
        let mut b = board_with_rom(vec![0xA5; 0x8000]);
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        // 32K image: offset $20000 (byte $8000) wraps back to byte 0.
        assert_eq!(b.read(0x0002_0000, 1, &mut host), 0xAF);
        // Anywhere below the 53C710 window still decodes ROM.
        assert_eq!(b.read(0x007F_FFFC, 1, &mut host), 0xAF);
    }

    #[test]
    fn dip_switches_read_back() {
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(b.read(DIP_OFFSET, 1, &mut host), 0xFF);
    }

    #[test]
    fn scratch_and_temp_written_via_the_shadow_read_back_at_the_base() {
        // The driver's walking-bits hardware test: write SCRATCH ($34) and
        // TEMP ($1C) through the +$40 write shadows, read back at the base.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        b.write(IO_OFFSET + 0x40 + 0x34, 4, 0xF0E7_C3A5, &mut host);
        b.write(IO_OFFSET + 0x40 + 0x1C, 4, 0xE1CF_874B, &mut host);
        assert_eq!(b.read(IO_OFFSET + 0x34, 4, &mut host), 0xF0E7_C3A5);
        assert_eq!(b.read(IO_OFFSET + 0x1C, 4, &mut host), 0xE1CF_874B);
    }

    #[test]
    fn ctest5_adck_and_bbck_strobe_the_counters_and_self_clear() {
        // The ncr7xx tool's register test: DBC=0x10 via a 32-bit DCMD write,
        // DNAD=0, then ADCK bumps DNAD to 4 (DBC untouched) and BBCK drops
        // DBC to 0xC; both bits read back clear.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        b.write(IO_OFFSET + 0x24, 4, 0x0000_0010, &mut host);
        b.write(IO_OFFSET + 0x28, 4, 0, &mut host);
        b.write(IO_OFFSET + 0x1A, 1, 0x80, &mut host);
        assert_eq!(b.read(IO_OFFSET + 0x28, 4, &mut host), 4);
        assert_eq!(b.read(IO_OFFSET + 0x24, 4, &mut host), 0x10);
        b.write(IO_OFFSET + 0x1A, 1, 0x40, &mut host);
        assert_eq!(b.read(IO_OFFSET + 0x24, 4, &mut host), 0xC);
        assert_eq!(b.read(IO_OFFSET + 0x1A, 1, &mut host) & 0xC0, 0);
    }

    #[test]
    fn dma_fifo_pushes_pops_with_parity_and_tracks_status() {
        // The ncr7xx DMA FIFO test: fill all four lanes through CTEST6
        // (parity from CTEST7.3), watching CTEST1/DSTAT.DFE, then drain
        // verifying data, parity in CTEST2.3, and status flags.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        let io = |r: usize| IO_OFFSET + r as u32;

        // ISTAT.RST software reset leaves the FIFO empty.
        b.write(io(REG_ISTAT), 1, 0x40, &mut host);
        b.write(io(REG_ISTAT), 1, 0, &mut host);
        assert_eq!(b.read(io(REG_CTEST1), 1, &mut host), 0xF0);
        assert_ne!(b.read(io(REG_DSTAT), 1, &mut host) & 0x80, 0);

        for lane in 0..4u32 {
            b.write(io(REG_CTEST4), 1, 0x04 | lane, &mut host);
            for byte in 0..16u32 {
                let parity = (lane + byte) & 1;
                b.write(io(REG_CTEST7), 1, parity << 3, &mut host);
                b.write(io(REG_CTEST6), 1, (0xA0 + byte) ^ lane, &mut host);
            }
        }
        // All lanes full, FIFO not empty.
        assert_eq!(b.read(io(REG_CTEST1), 1, &mut host), 0x0F);
        assert_eq!(b.read(io(REG_DSTAT), 1, &mut host) & 0x80, 0);

        for lane in 0..4u32 {
            b.write(io(REG_CTEST4), 1, 0x04 | lane, &mut host);
            for byte in 0..16u32 {
                let v = b.read(io(REG_CTEST6), 1, &mut host);
                assert_eq!(v, (0xA0 + byte) ^ lane, "lane {lane} byte {byte}");
                let parity = (b.read(io(REG_CTEST2), 1, &mut host) >> 3) & 1;
                assert_eq!(parity, (lane + byte) & 1, "parity lane {lane} byte {byte}");
            }
        }
        assert_eq!(b.read(io(REG_CTEST1), 1, &mut host), 0xF0);
        assert_ne!(b.read(io(REG_DSTAT), 1, &mut host) & 0x80, 0);
    }

    #[test]
    fn scsi_fifo_pushes_via_sodl_pops_via_ctest3_with_generated_parity() {
        // The ncr7xx SCSI FIFO test: SFWR routes SODL pushes to the FIFO,
        // parity generated odd (or even under SCNTL1.AESP), count in
        // SSTAT2's high nibble, pops through CTEST3 with parity in
        // CTEST2.SFP.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        let io = |r: usize| IO_OFFSET + r as u32;

        assert_eq!(b.read(io(REG_SSTAT2), 1, &mut host), 0x00);
        b.write(io(0x1B), 1, 0x08, &mut host); // CTEST4.SFWR
        let data = [0x00u8, 0x01, 0xFF, 0x5A, 0x81, 0x7E, 0x33, 0xC4];
        for (i, &d) in data.iter().enumerate() {
            // Alternate even/odd generation via SCNTL1.AESP.
            b.write(io(REG_SCNTL1), 1, ((i as u32) & 1) << 2, &mut host);
            assert_eq!(b.read(io(REG_SSTAT2), 1, &mut host) >> 4, i as u32);
            b.write(io(REG_SODL), 1, u32::from(d), &mut host);
        }
        assert_eq!(b.read(io(REG_SSTAT2), 1, &mut host), 0x80);

        for (i, &d) in data.iter().enumerate() {
            let v = b.read(io(REG_CTEST3), 1, &mut host);
            assert_eq!(v, u32::from(d), "byte {i}");
            let sfp = (b.read(io(REG_CTEST2), 1, &mut host) >> 4) & 1;
            let even = d.count_ones() & 1;
            let expect = if i & 1 == 1 { even } else { even ^ 1 };
            assert_eq!(sfp, expect, "parity byte {i}");
            assert_eq!(b.read(io(REG_SSTAT2), 1, &mut host) >> 4, (7 - i) as u32);
        }
    }

    #[test]
    fn ctest8_clf_drains_both_fifos() {
        // CTEST8.CLF clears the DMA FIFO (all four lanes) and the SCSI FIFO
        // in one strobe, and the bit itself does not latch.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        let io = |r: usize| IO_OFFSET + r as u32;

        // Push a byte into every DMA lane.
        for lane in 0..4u32 {
            b.write(io(REG_CTEST4), 1, 0x04 | lane, &mut host);
            b.write(io(REG_CTEST6), 1, 0xA0 + lane, &mut host);
        }
        // Push a byte into the SCSI FIFO.
        b.write(io(REG_CTEST4), 1, 0x08, &mut host); // SFWR
        b.write(io(REG_SODL), 1, 0x5A, &mut host);
        assert_eq!(b.read(io(REG_CTEST1), 1, &mut host), 0x00); // no lane empty
        assert_ne!(b.read(io(REG_SSTAT2), 1, &mut host) >> 4, 0);

        // CLF strobe drains everything and reads back clear.
        b.write(io(REG_CTEST8), 1, u32::from(CTEST8_CLF), &mut host);
        assert_eq!(b.read(io(REG_CTEST1), 1, &mut host), 0xF0); // all lanes empty
        assert_eq!(b.read(io(REG_SSTAT2), 1, &mut host) >> 4, 0);
        assert_eq!(
            b.read(io(REG_CTEST8), 1, &mut host) & u32::from(CTEST8_CLF),
            0
        );
    }

    #[test]
    fn scsi_pin_loopback_mirrors_sodl_socl_and_bus_reset() {
        // The ncr7xx SCSI pin test: in loopback (CTEST4.SLBE) with the data
        // bus asserted (SCNTL1.ADB), SODL mirrors onto SBDL and SOCL onto
        // SBCL, with the generated parity in SSTAT1.PAR; a SCNTL1.RST request
        // shows up in SSTAT1.RST.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        let io = |r: usize| IO_OFFSET + r as u32;

        // Idle bus: data and control lines read deasserted, no parity/reset.
        assert_eq!(b.read(io(REG_SBDL), 1, &mut host), 0);
        assert_eq!(b.read(io(REG_SBCL), 1, &mut host), 0);
        assert_eq!(b.read(io(REG_SSTAT1), 1, &mut host), 0);

        // A bus-reset request reflects into SSTAT1.RST, then clears.
        b.write(io(REG_SCNTL1), 1, u32::from(SCNTL1_RST), &mut host);
        assert_ne!(
            b.read(io(REG_SSTAT1), 1, &mut host) & u32::from(SSTAT1_RST),
            0
        );
        b.write(io(REG_SCNTL1), 1, 0, &mut host);
        assert_eq!(
            b.read(io(REG_SSTAT1), 1, &mut host) & u32::from(SSTAT1_RST),
            0
        );

        // Enter loopback with parity generation: SLBE + EPG + ADB.
        b.write(io(REG_CTEST4), 1, u32::from(CTEST4_SLBE), &mut host);
        b.write(io(REG_SCNTL0), 1, u32::from(SCNTL0_EPG), &mut host);
        b.write(io(REG_SCNTL1), 1, u32::from(SCNTL1_ADB), &mut host);

        // Data walk: SODL -> SBDL, odd generated parity, no control asserted.
        for d in [0x00u8, 0x01, 0x80, 0x5A, 0xFF] {
            b.write(io(REG_SODL), 1, u32::from(d), &mut host);
            assert_eq!(b.read(io(REG_SBDL), 1, &mut host), u32::from(d));
            assert_eq!(b.read(io(REG_SBCL), 1, &mut host), 0);
            let par = b.read(io(REG_SSTAT1), 1, &mut host) & u32::from(SSTAT1_PAR);
            let expect = u32::from((d.count_ones() & 1) == 0); // odd parity
            assert_eq!(par, expect, "parity for {d:#04x}");
        }

        // Control walk: SOCL -> SBCL, no stray data on the bus.
        b.write(io(REG_SODL), 1, 0, &mut host);
        for c in [0x01u8, 0x02, 0x04, 0x10, 0x20, 0x80] {
            b.write(io(REG_SOCL), 1, u32::from(c), &mut host);
            assert_eq!(
                b.read(io(REG_SBCL), 1, &mut host),
                u32::from(c),
                "ctrl {c:#04x}"
            );
            assert_eq!(b.read(io(REG_SBDL), 1, &mut host), 0);
        }
        // ACK (bit 3) stays deasserted until target mode (SCNTL0.TRG).
        b.write(io(REG_SOCL), 1, 0x08, &mut host);
        assert_eq!(b.read(io(REG_SBCL), 1, &mut host), 0);
        b.write(
            io(REG_SCNTL0),
            1,
            u32::from(SCNTL0_EPG | SCNTL0_TRG),
            &mut host,
        );
        assert_eq!(b.read(io(REG_SBCL), 1, &mut host), 0x08);
    }

    #[test]
    fn istat_abort_latches_dstat_and_raises_int2_until_dstat_read() {
        // The ncr7xx bus access test, stage 1: DIEN.ABRT + ISTAT.ABRT must
        // pull INT2 with ISTAT showing DIP|ABRT; the ISR's DSTAT read
        // returns ABRT|DFE and drops the line.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        let io = |r: usize| IO_OFFSET + r as u32;

        b.write(io(REG_DIEN), 1, 0x10, &mut host);
        assert!(!ZorroDevice::int2_line(&b));
        b.write(io(REG_ISTAT), 1, 0x80, &mut host);
        assert!(ZorroDevice::int2_line(&b));
        assert_eq!(b.read(io(REG_ISTAT), 1, &mut host), 0x81); // ABRT|DIP
        assert_eq!(b.read(io(REG_DSTAT), 1, &mut host), 0x90); // DFE|ABRT
        assert!(!ZorroDevice::int2_line(&b));
        assert_eq!(b.read(io(REG_ISTAT), 1, &mut host), 0x80); // DIP gone
        b.write(io(REG_ISTAT), 1, 0, &mut host);
        assert_eq!(b.read(io(REG_ISTAT), 1, &mut host), 0x00);
    }

    #[test]
    fn disabled_abort_still_sets_dip_but_not_the_line() {
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        let io = |r: usize| IO_OFFSET + r as u32;
        b.write(io(REG_ISTAT), 1, 0x80, &mut host);
        assert!(!ZorroDevice::int2_line(&b));
        assert_eq!(b.read(io(REG_ISTAT), 1, &mut host) & 1, 1);
    }

    #[test]
    fn scripts_write_register_then_interrupt() {
        // The ncr7xx bus access test's probe script: MOVE $A5 to DWT
        // (register $3A chip-native = $39 as the 68k sees it), then
        // INT $00000012.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        mem.chip_ram[0x1000..0x1010].copy_from_slice(&[
            0x78, 0x3A, 0xA5, 0x00, 0x00, 0x00, 0x00, 0x00, // MOVE $A5 to DWT
            0x98, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12, // INT $12
        ]);
        let mut host = DeviceHost::new(&mut mem);
        let io = |r: usize| IO_OFFSET + r as u32;
        b.write(io(REG_DIEN), 1, 0x04, &mut host); // enable SIR
        b.write(io(REG_DSP), 4, 0x1000, &mut host);
        assert_eq!(b.read(io(0x39), 1, &mut host), 0xA5); // DWT written
        assert!(ZorroDevice::int2_line(&b));
        assert_eq!(b.read(io(REG_DSPS), 4, &mut host), 0x12);
        assert_eq!(b.read(io(REG_DSTAT), 1, &mut host) & 0x04, 0x04); // SIR
        assert!(!ZorroDevice::int2_line(&b));
    }

    #[test]
    fn scripts_garbage_fetch_raises_illegal_instruction() {
        // Fetching from unmapped space yields $FF bytes; the engine must
        // stop with DSTAT.IID like the real chip on a garbage opcode.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        let io = |r: usize| IO_OFFSET + r as u32;
        b.write(io(REG_DSP), 4, 0x0F00_0000, &mut host);
        assert_eq!(b.read(io(REG_DSTAT), 1, &mut host) & 0x01, 0x01); // IID
    }

    #[test]
    fn scripts_memory_move_copies_own_scratch_to_temp() {
        // ncr7xx DMA test 1: Memory Move of the chip's own SCRATCH to TEMP
        // through the configured board window, then INT.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        mem.zorro
            .add_board_configured_at(crate::zorro::BoardSpec::a4091(0), 0x4200_0000)
            .unwrap();
        mem.chip_ram[0x1000..0x1014].copy_from_slice(&[
            0xC0, 0x00, 0x00, 0x04, // Memory Move, 4 bytes
            0x42, 0x80, 0x00, 0x34, // src: SCRATCH via the board window
            0x42, 0x80, 0x00, 0x1C, // dst: TEMP via the board window
            0x98, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // INT
        ]);
        let mut host = DeviceHost::for_slot(&mut mem, 0);
        let io = |r: usize| IO_OFFSET + r as u32;
        b.write(io(0x34), 4, 0xCAE5_92F7, &mut host); // SCRATCH
        b.write(io(REG_TEMP), 4, !0xCAE5_92F7, &mut host);
        b.write(io(REG_DSP), 4, 0x1000, &mut host);
        assert_eq!(b.read(io(REG_TEMP), 4, &mut host), 0xCAE5_92F7);
        assert_eq!(b.read(io(REG_DSTAT), 1, &mut host) & 0x04, 0x04); // SIR
    }

    #[test]
    fn scripts_memory_move_ram_to_ram() {
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        mem.chip_ram[0x2000..0x2008].copy_from_slice(b"COPPRLNE");
        mem.chip_ram[0x1000..0x1014].copy_from_slice(&[
            0xC0, 0x00, 0x00, 0x08, // Memory Move, 8 bytes
            0x00, 0x00, 0x20, 0x00, // src
            0x00, 0x00, 0x30, 0x00, // dst
            0x98, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // INT
        ]);
        let mut host = DeviceHost::for_slot(&mut mem, 0);
        let io = |r: usize| IO_OFFSET + r as u32;
        b.write(io(REG_DSP), 4, 0x1000, &mut host);
        assert_eq!(b.read(io(REG_DSTAT), 1, &mut host) & 0x04, 0x04);
        assert_eq!(&host.memory_mut().chip_ram[0x3000..0x3008], b"COPPRLNE");
    }

    #[test]
    fn ctest8_reports_chip_revision_2() {
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(b.read(IO_OFFSET + 0x21, 1, &mut host) >> 4, 2);
    }

    #[test]
    fn peek_serves_rom_and_leaves_io_opaque() {
        let b = board_with_rom(test_rom_64k());
        assert_eq!(b.peek_word(0x48), Some(0x1FFF));
        assert_eq!(b.peek_word(IO_OFFSET), None);
    }
}
