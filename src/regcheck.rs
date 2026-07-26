// SPDX-License-Identifier: GPL-3.0-or-later

//! Custom-register access validator: a running report of software using
//! the chipset in ways the hardware does not reward.
//!
//! Every custom-register access in Copperline funnels through one place
//! (`Bus::custom_write` and `Bus::write_custom_word_from`), which makes it
//! cheap to notice the classic misuses: writing a register the fitted
//! chipset does not have, writing bits a register does not define,
//! reading a write-only register, byte or odd-address access to a word
//! register, and pointing a DMA channel outside the RAM Agnus can reach.
//! None of those are errors the machine reports -- the write simply lands
//! nowhere, or somewhere unintended -- so the effect shows up much later
//! as "the display is wrong" with nothing to bisect.
//!
//! Each finding names the offending PC (or Copper address) and the beam
//! position, so it points at an instruction rather than at a symptom. The
//! checks describe 68000/Agnus/Denise/Paula behaviour only: nothing here
//! knows what program is running.
//!
//! Findings are deduplicated by (kind, register, writer) and counted, so
//! a Copper list repeating the same mistake every frame produces one
//! entry with a tally rather than fifty thousand log lines.

use std::collections::BTreeMap;

/// Findings retained before the oldest is dropped. Generous enough for
/// any real report; bounded so a pathological program cannot grow the
/// bus without limit.
pub const MAX_FINDINGS: usize = 256;

/// What kind of hardware misuse was seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Finding {
    /// A register the fitted Agnus/Denise does not implement. The write
    /// is accepted by the address decode and then dropped.
    AbsentRegister,
    /// A read-only register was written, or a write-only register read.
    /// The access goes nowhere; a write-only read returns the undriven
    /// bus rather than the latch.
    WrongDirection,
    /// Bits with no defined function were set. Real silicon ignores
    /// them, but they are almost always a sign of a miscomputed value,
    /// and some become real bits on a later chipset.
    UnusedBits,
    /// A byte-sized access to a word register. The data bus mirrors the
    /// byte into both halves, so the register does not receive the value
    /// the code appears to be writing.
    ByteAccess,
    /// A word access at an odd address.
    OddAddress,
    /// A DMA pointer was set outside the chip RAM Agnus can address, so
    /// the channel will fetch from wrapped or unbacked addresses.
    PointerOutsideChipRam,
    /// An access above the $000-$1FE register bank. Nothing decodes
    /// there: the write is dropped and the read returns the undriven bus.
    UnmappedOffset,
    /// A blit was started while the previous one was still running. On
    /// hardware the CPU stalls until the blitter frees the register file,
    /// and with BLTPRI set it can be locked out for the whole blit.
    BlitterBusy,
    /// A blit was started with the DMA that runs it switched off, so it
    /// never progresses and its completion interrupt never fires -- the
    /// classic wait-for-BBUSY hang.
    BlitterDmaOff,
    /// Disk DMA was armed against a drive that cannot serve it (none
    /// selected, motor off, or no media). Nothing arrives, and a loader
    /// waiting on DSKBLK waits forever.
    DiskNotReady,
    /// A keyboard handshake pulse too short for the 6500/1 to sample.
    KeyboardHandshakeShort,
}

impl Finding {
    pub fn name(self) -> &'static str {
        match self {
            Finding::AbsentRegister => "absent-register",
            Finding::WrongDirection => "wrong-direction",
            Finding::UnusedBits => "unused-bits",
            Finding::ByteAccess => "byte-access",
            Finding::OddAddress => "odd-address",
            Finding::PointerOutsideChipRam => "pointer-outside-chip-ram",
            Finding::UnmappedOffset => "unmapped-offset",
            Finding::BlitterBusy => "blitter-busy",
            Finding::BlitterDmaOff => "blitter-dma-off",
            Finding::DiskNotReady => "disk-not-ready",
            Finding::KeyboardHandshakeShort => "keyboard-handshake-short",
        }
    }
}

/// Who made the access, for attribution in a finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Writer {
    /// The CPU, at this PC.
    Cpu(u32),
    /// The Copper, at this list address.
    Copper(u32),
}

impl Writer {
    pub fn label(self) -> &'static str {
        match self {
            Writer::Cpu(_) => "cpu",
            Writer::Copper(_) => "copper",
        }
    }

    pub fn address(self) -> u32 {
        match self {
            Writer::Cpu(pc) | Writer::Copper(pc) => pc,
        }
    }
}

/// One deduplicated finding and everything needed to report it.
#[derive(Clone, Copy, Debug)]
pub struct Report {
    pub finding: Finding,
    /// Custom-register word offset ($000-$1FE).
    pub reg: u16,
    pub writer: Writer,
    /// The value written (or read), and for `UnusedBits` the offending
    /// bits alone.
    pub value: u16,
    pub detail: u16,
    /// Beam position of the first occurrence.
    pub vpos: u16,
    pub hpos: u16,
    /// How many times this (kind, register, writer) has been seen.
    pub count: u64,
}

/// The validator's accumulated report.
#[derive(Clone, Debug, Default)]
pub struct RegCheck {
    seen: BTreeMap<(Finding, u16, Writer), Report>,
    /// Findings dropped because the report was full, so a reader can
    /// tell a complete report from a truncated one.
    pub dropped: u64,
}

impl RegCheck {
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    pub fn clear(&mut self) {
        self.seen.clear();
        self.dropped = 0;
    }

    /// Record one finding, merging it into an existing entry for the
    /// same (kind, register, writer).
    #[allow(clippy::too_many_arguments)]
    pub fn note(
        &mut self,
        finding: Finding,
        reg: u16,
        writer: Writer,
        value: u16,
        detail: u16,
        vpos: u16,
        hpos: u16,
    ) -> bool {
        let key = (finding, reg, writer);
        if let Some(entry) = self.seen.get_mut(&key) {
            entry.count += 1;
            return false;
        }
        if self.seen.len() >= MAX_FINDINGS {
            self.dropped += 1;
            return false;
        }
        self.seen.insert(
            key,
            Report {
                finding,
                reg,
                writer,
                value,
                detail,
                vpos,
                hpos,
                count: 1,
            },
        );
        true
    }

    /// Every finding, most-repeated first.
    pub fn reports(&self) -> Vec<Report> {
        let mut out: Vec<Report> = self.seen.values().copied().collect();
        out.sort_by(|a, b| b.count.cmp(&a.count).then(a.reg.cmp(&b.reg)));
        out
    }

    /// One human-readable line for a finding, in the shape the log and
    /// the debugger console both print.
    pub fn describe(report: &Report) -> String {
        let who = report.writer.label();
        let at = report.writer.address();
        let times = match report.count {
            1 => String::new(),
            n => format!(", {n} times"),
        };
        // The keyboard handshake is driven through CIA-A's serial port,
        // not a custom register, so it does not name one.
        if report.finding == Finding::KeyboardHandshakeShort {
            return format!(
                "keyboard handshake pulse of {} cck is under the {} cck floor that \
                 separates a deliberate pulse from an incidental CIA reconfiguration, \
                 and the MCU was waiting for one; it resynchronises after 143 ms and \
                 resends with $F9, so a key is lost and input stalls until then \
                 (by {who} at {at:#08X}, v{} h{}{times})",
                report.value, report.detail, report.vpos, report.hpos,
            );
        }
        let reg = crate::debugger::custom_reg_name(report.reg);
        let what = match report.finding {
            Finding::AbsentRegister => format!(
                "{reg} is not present on the fitted chipset; write {:#06X} is dropped",
                report.value
            ),
            Finding::WrongDirection => format!(
                "{reg} accessed against its direction; a write to a read-only register \
                 lands nowhere, and a read of a write-only one returns the undriven bus"
            ),
            Finding::UnusedBits => format!(
                "{reg} write {:#06X} sets undefined bits {:#06X}",
                report.value, report.detail
            ),
            Finding::ByteAccess => format!(
                "{reg} accessed a byte at a time; the custom chips have no byte lanes, \
                 so a byte write latches into both halves of the register"
            ),
            Finding::OddAddress => format!("{reg} accessed at an odd address"),
            Finding::PointerOutsideChipRam => format!(
                "{reg} aimed at {:#08X}, past the chip RAM Agnus can address; \
                 the pointer wraps",
                u32::from(report.detail) << 16
            ),
            Finding::UnmappedOffset => format!(
                "${:03X} is above the $000-$1FE custom register bank; nothing decodes \
                 there, so the access reaches no register",
                report.reg
            ),
            Finding::BlitterBusy => format!(
                "{reg} started a blit while the previous one was still running; the CPU \
                 stalls until the blitter frees its registers"
            ),
            Finding::BlitterDmaOff => format!(
                "{reg} started a blit with DMACON BLTEN/DMAEN clear ({:#06X}); it will \
                 never run or raise its completion interrupt",
                report.detail
            ),
            Finding::DiskNotReady => {
                format!("{reg} armed disk DMA but {}", disk_obstacle(report.detail))
            }
            Finding::KeyboardHandshakeShort => unreachable!("handled above"),
        };
        format!(
            "{what} (by {who} at {at:#08X}, v{} h{}{times})",
            report.vpos, report.hpos
        )
    }
}

/// Codes for [`Finding::DiskNotReady`]'s `detail`, so the report stays a
/// plain Copy struct rather than carrying a string per finding.
pub const DISK_NO_DRIVE: u16 = 0;
pub const DISK_MOTOR_OFF: u16 = 1;
pub const DISK_EMPTY: u16 = 2;

/// The reason code as the report prints it. Kept in step with
/// `FloppyController::dma_arming_obstacle`.
pub fn disk_obstacle(code: u16) -> &'static str {
    match code {
        DISK_MOTOR_OFF => "the selected drive's motor is off",
        DISK_EMPTY => "the selected drive is empty",
        _ => "no drive is selected",
    }
}

/// Bits each writable custom register actually defines. A register with
/// no entry is not checked for undefined bits: the omission means "every
/// bit of this register is data" (pointers, modulos, palette entries,
/// sprite position words, blitter data) or that the field layout is
/// resolution- or revision-dependent in a way a fixed mask would
/// misreport.
///
/// Masks are the OCS/ECS/AGA union: a bit that exists on any fitted
/// revision is defined, because the absent-register check already covers
/// writing to hardware that is not there, and flagging an AGA bit on an
/// AGA machine would be wrong.
pub fn defined_bits(reg: u16) -> Option<u16> {
    Some(match reg {
        0x02E => 0x0002, // COPCON: CDANG only
        0x034 => 0xFF01, // POTGO: OUTRY/DATRY..OUTLX/DATLX plus START
        0x040 => 0xFFFF, // BLTCON0: ASH, USE mask, LF
        // BLTCON1: LINE, DESC/SING, FCI/AUL, IFE/SUL, EFE/SUD, SIGN,
        // DOFF (ECS), BSH/texture. Bits 5 and 8-11 have no function --
        // 8-11 are BLTCON0's USE bits, not this register's.
        0x042 => 0xF0DF,
        0x058 => 0xFFFF,         // BLTSIZE: h9-0, w5-0
        0x05A => 0x00FF,         // BLTCON0L (ECS): the LF byte only
        0x05C => 0x7FFF,         // BLTSIZV (ECS): 15-bit height
        0x05E => 0x07FF,         // BLTSIZH (ECS): 11-bit width
        0x08E | 0x090 => 0xFFFF, // DIWSTRT/DIWSTOP: V and H bytes
        // DDFSTRT/DDFSTOP: H8-H2. OCS decodes to 4-cck granularity and
        // ECS/AGA to 2, but neither implements bit 0.
        0x092 | 0x094 => 0x00FE,
        // DMACON: SET/CLR plus BLTPRI..AUD0EN. BBUSY (14) and BZERO (13)
        // are read-only status seen through DMACONR, and 11-12 are unused;
        // Agnus masks writes to the low 11 bits.
        0x096 => 0x87FF,
        0x098 => 0xFFFF, // CLXCON: ENSP/ENBP and their match values
        0x09A => 0xFFFF, // INTENA: SET/CLR, INTEN, and the 14 sources
        0x09C => 0xBFFF, // INTREQ: the same less INTEN, which is INTENA's alone
        0x09E => 0xFFFF, // ADKCON: SET/CLR plus PRECOMP..USE0V1
        0x100 => 0xFFFF, // BPLCON0: HIRES..ECSENA (BPU3/SHRES/UHRES on ECS/AGA)
        0x102 => 0xFFFF, // BPLCON1: PF1/PF2 scroll (8-bit fields on AGA)
        0x104 => 0x7FFF, // BPLCON2: ZDCTL..PF2P0; bit 15 undefined
        // BPLCON3 (ECS/AGA): BANK, PF2OF, LOCT, SPRES, BRDRBLNK,
        // BRDNTRAN, ZDCLKEN, BRDRSPRT, EXTBLKEN. Bits 3 and 8 have no
        // function on either revision.
        0x106 => 0xFEF7,
        0x10C => 0xFFFF, // BPLCON4 (AGA): BPLAM, ESPRM, OSPRM
        // CLXCON2 (AGA): ENBP7/ENBP8 (6-7) and MVBP7/MVBP8 (0-1) only,
        // which is exactly what the renderer decodes.
        0x10E => 0x00C3,
        // BEAMCON0 (ECS): HARDDIS (14) down to the colour-burst enable;
        // bit 15 has no function. (ERSY is BPLCON0 bit 1 -- a different
        // chip's register entirely.)
        0x1DC => 0x7FFF,
        0x1FC => 0xC00F, // FMODE (AGA): SSCAN2/BSCAN2, SPAGEM/SPR32, BPAGEM/BPL32
        _ => return None,
    })
}

/// Registers that only read (the CPU writing one is misuse) and
/// registers that only write (the CPU reading one gets the undriven bus,
/// not the latch).
pub fn direction(reg: u16) -> Option<Direction> {
    Some(match reg {
        0x000 => Direction::ReadOnly,         // BLTDDAT
        0x002 => Direction::ReadOnly,         // DMACONR
        0x004 | 0x006 => Direction::ReadOnly, // VPOSR/VHPOSR (VPOSW/VHPOSW are $02A/$02C)
        0x008 => Direction::ReadOnly,         // DSKDATR
        0x00A | 0x00C => Direction::ReadOnly, // JOY0DAT/JOY1DAT
        0x00E => Direction::ReadOnly,         // CLXDAT
        0x010 => Direction::ReadOnly,         // ADKCONR
        0x012 | 0x014 => Direction::ReadOnly, // POT0DAT/POT1DAT
        0x016 => Direction::ReadOnly,         // POTGOR
        0x018 => Direction::ReadOnly,         // SERDATR
        0x01A => Direction::ReadOnly,         // DSKBYTR
        0x01C | 0x01E => Direction::ReadOnly, // INTENAR/INTREQR
        0x07C => Direction::ReadOnly,         // DENISEID
        // HHPOSR: the ECS UHRES counter readback, paired with HHPOSW at
        // $1D8. It sits inside the otherwise write-only tail, and the
        // emulator does serve guest reads of it.
        0x1DA => Direction::ReadOnly,
        // Everything from the strobes upward is write-only; the ones with
        // a read counterpart at another offset are listed above.
        0x020..=0x03E | 0x040..=0x07A | 0x07E..=0x1BE | 0x1C0..=0x1FE => Direction::WriteOnly,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    ReadOnly,
    WriteOnly,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_findings_merge_into_one_entry_with_a_tally() {
        let mut check = RegCheck::default();
        assert!(check.note(
            Finding::UnusedBits,
            0x104,
            Writer::Cpu(0x100),
            0x8000,
            0x8000,
            10,
            20
        ));
        assert!(!check.note(
            Finding::UnusedBits,
            0x104,
            Writer::Cpu(0x100),
            0x8000,
            0x8000,
            11,
            20
        ));
        let reports = check.reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].count, 2);
        // The first occurrence's beam position is the one kept: it is the
        // one worth breakpointing on.
        assert_eq!(reports[0].vpos, 10);
    }

    #[test]
    fn a_different_writer_is_a_different_finding() {
        let mut check = RegCheck::default();
        check.note(
            Finding::UnusedBits,
            0x104,
            Writer::Cpu(0x100),
            0x8000,
            0x8000,
            0,
            0,
        );
        check.note(
            Finding::UnusedBits,
            0x104,
            Writer::Copper(0x200),
            0x8000,
            0x8000,
            0,
            0,
        );
        assert_eq!(check.reports().len(), 2);
    }

    #[test]
    fn the_report_is_bounded_and_says_when_it_truncated() {
        let mut check = RegCheck::default();
        for pc in 0..(MAX_FINDINGS as u32 + 10) {
            check.note(Finding::UnusedBits, 0x104, Writer::Cpu(pc), 1, 1, 0, 0);
        }
        assert_eq!(check.reports().len(), MAX_FINDINGS);
        assert_eq!(check.dropped, 10);
    }

    #[test]
    fn read_only_and_write_only_registers_are_classified() {
        assert_eq!(direction(0x002), Some(Direction::ReadOnly)); // DMACONR
        assert_eq!(direction(0x096), Some(Direction::WriteOnly)); // DMACON
        assert_eq!(direction(0x004), Some(Direction::ReadOnly)); // VPOSR
        assert_eq!(direction(0x02A), Some(Direction::WriteOnly)); // VPOSW
        assert_eq!(direction(0x180), Some(Direction::WriteOnly)); // COLOR00
    }

    #[test]
    fn undefined_bit_masks_cover_the_control_registers_only() {
        // BPLCON2 bit 15 has no function on any revision.
        assert_eq!(defined_bits(0x104), Some(0x7FFF));
        // Pointers, modulos and palette entries are all data: no mask, so
        // no false "undefined bit" report on a legitimate value.
        assert_eq!(defined_bits(0x0E0), None);
        assert_eq!(defined_bits(0x108), None);
        assert_eq!(defined_bits(0x180), None);
    }

    #[test]
    fn descriptions_name_the_register_the_writer_and_the_repeat_count() {
        let mut check = RegCheck::default();
        check.note(
            Finding::UnusedBits,
            0x104,
            Writer::Copper(0x7C00),
            0x8020,
            0x8000,
            42,
            100,
        );
        check.note(
            Finding::UnusedBits,
            0x104,
            Writer::Copper(0x7C00),
            0x8020,
            0x8000,
            43,
            100,
        );
        let line = RegCheck::describe(&check.reports()[0]);
        assert!(line.contains("BPLCON2"), "{line}");
        assert!(line.contains("undefined bits 0x8000"), "{line}");
        assert!(line.contains("by copper at 0x007C00"), "{line}");
        assert!(line.contains("2 times"), "{line}");
    }
}
