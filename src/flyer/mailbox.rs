// SPDX-License-Identifier: GPL-3.0-or-later

//! The Flyer's command mailbox: the layout of the shared SRAM, the eight
//! command slots, the opcode and error numbers, and which slots a write to
//! the window touched.
//!
//! Everything flyer.library asks of the card goes through eight 32-byte
//! slots at `$500`. A slot's first word is the opcode, its top two bits the
//! status: `00` blank, `01` new, `10` in progress, `11` done. The library
//! takes a free slot by writing `$0001` ("taken", still blank), clears the
//! other 30 bytes, fills in the arguments and writes `opcode | $4000` last
//! (`FindFreeCmd`/`FireCmd` in `flyerlib.asm`). The firmware's main loop
//! marks a new command in progress, which also clears its error byte, runs
//! it and ORs in `$C000` (`MainLoop`, `CommandBusy`, `CommandDone` in the
//! firmware's `amishar.c`). The library polls for done, copies the results
//! back and writes 0 to free the slot.
//!
//! Byte 2 is the `cont` flag: 1 = go, 0 = abort, `$FF` = pause from the
//! Amiga, 2 = "started" from the Flyer. Byte 3 is the error code, bytes
//! 4-31 the arguments. Pointers in the arguments are SRAM offsets, never
//! Amiga addresses.

/// The shared SRAM, and with it the board's Zorro II window: 2 x 128K x 8.
pub const SRAM_SIZE: usize = 0x4_0000;
/// The firmware's crash report (`$DEAD`, trap, PC, registers). Written only
/// when the Flyer's 68000 crashes, which the HLE never does.
pub const GURU: usize = 0x400;
/// Sequencing status the Amiga reads without a command: `+0` SeqError.b,
/// `+1` Done.b, `+2` UserID.l, `+6` MoreInfo.l.
pub const SHARED_CTRL: usize = 0x480;
/// The first command slot.
pub const CMD_BASE: usize = 0x500;
/// Number of command slots.
pub const CMD_SLOTS: usize = 8;
/// Bytes per command slot.
pub const CMD_LEN: usize = 32;
/// End of the pool the library manages (`$600` up to here); the firmware's
/// own buffers start above it.
pub const SHARED_TOP: usize = 0xF000;
/// Where the library puts Flyer.bin and tells the boot ROM to run it.
pub const PROG_AREA: usize = 0x1_8000;
/// The Flyer CPU's address of SRAM byte 0. Test tools pass Flyer CPU
/// addresses (CPUR/CPUW); everything else passes SRAM offsets.
pub const SRAM_CPU_BASE: u32 = 0x00C0_0000;

/// Status bits in a slot's opcode word.
pub const STAT_MASK: u16 = 0xC000;
pub const STAT_BLANK: u16 = 0x0000;
pub const STAT_NEW: u16 = 0x4000;
pub const STAT_INPROG: u16 = 0x8000;
pub const STAT_DONE: u16 = 0xC000;
/// The opcode under the status bits.
pub const OPCODE_MASK: u16 = 0x3FFF;

/// Opcode numbers: positions in `opcodes.i` (the library) and in the
/// firmware's `FuncTable` (`amishar.c`), which agree.
#[allow(dead_code)]
pub mod op {
    pub const NULL: u16 = 0x00;
    pub const FIRMWARE: u16 = 0x01;
    pub const CALLMOD: u16 = 0x02;
    pub const PGMFPGA: u16 = 0x03;
    pub const SBUSW: u16 = 0x04;
    pub const SBUSR: u16 = 0x05;
    pub const CPUW: u16 = 0x06;
    pub const CPUR: u16 = 0x07;
    pub const GETCLOCK: u16 = 0x08;
    pub const FIRINIT: u16 = 0x09;
    pub const FIRXCHG: u16 = 0x0A;
    pub const FIRMAPRAM: u16 = 0x0B;
    pub const DSPBOOT: u16 = 0x0C;
    pub const CPUDMA: u16 = 0x0D;
    pub const VIDPARAM: u16 = 0x0E;
    pub const PLAYMODE: u16 = 0x0F;
    pub const RECMODE: u16 = 0x10;
    pub const NOMODE: u16 = 0x11;
    pub const TOASTMUX: u16 = 0x12;
    pub const INPUTSEL: u16 = 0x13;
    pub const TERMINATE: u16 = 0x14;
    pub const SRCHARGS: u16 = 0x15;
    pub const DOSEARCH: u16 = 0x16;
    pub const FLYPLAY: u16 = 0x17;
    pub const FLYRECORD: u16 = 0x18;
    pub const CHGAUDIO: u16 = 0x19;
    pub const GETSMPTE: u16 = 0x1A;
    pub const NEWHEADLIST: u16 = 0x1B;
    pub const ENDHEADLIST: u16 = 0x1C;
    pub const MAKEHEAD: u16 = 0x1D;
    pub const VOIDHEAD: u16 = 0x1E;
    pub const VOIDALL: u16 = 0x1F;
    pub const AUDEKEY: u16 = 0x20;
    pub const SCSIRST: u16 = 0x21;
    pub const SCSIINIT: u16 = 0x22;
    pub const FINDDRV: u16 = 0x23;
    pub const COPYDATA: u16 = 0x24;
    pub const SCSICALL: u16 = 0x25;
    pub const READSIZE: u16 = 0x26;
    pub const READ10: u16 = 0x27;
    pub const WRITE10: u16 = 0x28;
    pub const DRIVEINFO: u16 = 0x29;
    pub const LOCATE: u16 = 0x2A;
    pub const FREEGRIP: u16 = 0x2B;
    pub const COPYGRIP: u16 = 0x2C;
    pub const CMPGRIPS: u16 = 0x2D;
    pub const PARENT: u16 = 0x2E;
    pub const EXAMINE: u16 = 0x2F;
    pub const DIRLIST: u16 = 0x30;
    pub const FILEINFO: u16 = 0x31;
    pub const FILEOPEN: u16 = 0x32;
    pub const FILECLOSE: u16 = 0x33;
    pub const FILESEEK: u16 = 0x34;
    pub const FILEREAD: u16 = 0x35;
    pub const FILEWRITE: u16 = 0x36;
    pub const CREATEDIR: u16 = 0x37;
    pub const DELETE: u16 = 0x38;
    pub const RENAME: u16 = 0x39;
    pub const RENAMEDISK: u16 = 0x3A;
    pub const FORMAT: u16 = 0x3B;
    pub const DEFRAG: u16 = 0x3C;
    pub const SETBITS: u16 = 0x3D;
    pub const SETDATE: u16 = 0x3E;
    pub const SETCOMMENT: u16 = 0x3F;
    pub const WRITEPROT: u16 = 0x40;
    pub const CHANGEMODE: u16 = 0x41;
    pub const MAKEFILE: u16 = 0x42;
    pub const COPYCLIP: u16 = 0x43;
    pub const DEBUGFLAGS: u16 = 0x44;
    pub const FLOOBY: u16 = 0x45;
    pub const READTEST: u16 = 0x46;
    pub const WRITETEST: u16 = 0x47;
    pub const QUIT: u16 = 0x48;
    pub const SCSIDIRECT: u16 = 0x49;
    pub const OPENREADFLD: u16 = 0x4A;
    pub const OPENWRITEFLD: u16 = 0x4B;
    pub const CLOSEFIELD: u16 = 0x4C;
    pub const READLINE: u16 = 0x4D;
    pub const WRITELINE: u16 = 0x4E;
    pub const SETFILLCOLOR: u16 = 0x4F;
    pub const SKIPLINES: u16 = 0x50;
    pub const GETCOMPINFO: u16 = 0x51;
    pub const SETCLOCK: u16 = 0x52;
    pub const STRIPAUDIO: u16 = 0x53;
    pub const WRITECALIB: u16 = 0x54;
    pub const READCALIB: u16 = 0x55;
    pub const EEWRITE: u16 = 0x56;
    pub const EEREAD: u16 = 0x57;
    pub const RESETFLYER: u16 = 0x58;
    pub const SETCLOCKGEN: u16 = 0x59;
    pub const LOADVID: u16 = 0x5A;
    pub const SERDEVICE: u16 = 0x5B;
    pub const SELFTEST: u16 = 0x5C;
    pub const TBC: u16 = 0x5D;
    pub const NEWCUTLIST: u16 = 0x5E;
    pub const ADDSUBCLIP: u16 = 0x5F;
    pub const MAKESUBCLIPS: u16 = 0x60;
    pub const CODECCTRL: u16 = 0x61;
    pub const NEWSEQ: u16 = 0x62;
    pub const ADDSEQCLIP: u16 = 0x63;
    pub const ENDSEQ: u16 = 0x64;
    pub const PLAYSEQ: u16 = 0x65;
    pub const GETSETOPT: u16 = 0x66;
    pub const LOCATEFLD: u16 = 0x67;
    pub const CACHETEST: u16 = 0x68;
    pub const FILEEXTEND: u16 = 0x69;
    pub const FILEHSREAD: u16 = 0x6A;
    pub const FILEHSWRITE: u16 = 0x6B;
    /// Entries in the firmware's `FuncTable`; anything at or above is
    /// answered `BADCOMMAND` by its main loop.
    pub const COUNT: u16 = 0x6C;
}

/// The opcode names, for the log (the library's `op_` names).
const OPCODE_NAMES: [&str; op::COUNT as usize] = [
    "NULL",
    "FIRMWARE",
    "CALLMOD",
    "PGMFPGA",
    "SBUSW",
    "SBUSR",
    "CPUW",
    "CPUR",
    "GETCLOCK",
    "FIRINIT",
    "FIRXCHG",
    "FIRMAPRAM",
    "DSPBOOT",
    "CPUDMA",
    "VIDPARAM",
    "PLAYMODE",
    "RECMODE",
    "NOMODE",
    "TOASTMUX",
    "INPUTSEL",
    "TERMINATE",
    "SRCHARGS",
    "DOSEARCH",
    "FLYPLAY",
    "FLYRECORD",
    "CHGAUDIO",
    "GETSMPTE",
    "NEWHEADLIST",
    "ENDHEADLIST",
    "MAKEHEAD",
    "VOIDHEAD",
    "VOIDALL",
    "AUDEKEY",
    "SCSIRST",
    "SCSIINIT",
    "FINDDRV",
    "COPYDATA",
    "SCSICALL",
    "READSIZE",
    "READ10",
    "WRITE10",
    "DRIVEINFO",
    "LOCATE",
    "FREEGRIP",
    "COPYGRIP",
    "CMPGRIPS",
    "PARENT",
    "EXAMINE",
    "DIRLIST",
    "FILEINFO",
    "FILEOPEN",
    "FILECLOSE",
    "FILESEEK",
    "FILEREAD",
    "FILEWRITE",
    "CREATEDIR",
    "DELETE",
    "RENAME",
    "RENAMEDISK",
    "FORMAT",
    "DEFRAG",
    "SETBITS",
    "SETDATE",
    "SETCOMMENT",
    "WRITEPROT",
    "CHANGEMODE",
    "MAKEFILE",
    "COPYCLIP",
    "DEBUGFLAGS",
    "FLOOBY",
    "READTEST",
    "WRITETEST",
    "QUIT",
    "SCSIDIRECT",
    "OPENREADFLD",
    "OPENWRITEFLD",
    "CLOSEFIELD",
    "READLINE",
    "WRITELINE",
    "SETFILLCOLOR",
    "SKIPLINES",
    "GETCOMPINFO",
    "SETCLOCK",
    "STRIPAUDIO",
    "WRITECALIB",
    "READCALIB",
    "EEWRITE",
    "EEREAD",
    "RESETFLYER",
    "SETCLOCKGEN",
    "LOADVID",
    "SERDEVICE",
    "SELFTEST",
    "TBC",
    "NEWCUTLIST",
    "ADDSUBCLIP",
    "MAKESUBCLIPS",
    "CODECCTRL",
    "NEWSEQ",
    "ADDSEQCLIP",
    "ENDSEQ",
    "PLAYSEQ",
    "GETSETOPT",
    "LOCATEFLD",
    "CACHETEST",
    "FILEEXTEND",
    "FILEHSREAD",
    "FILEHSWRITE",
];

/// The log name of an opcode.
pub fn opcode_name(opcode: u16) -> &'static str {
    OPCODE_NAMES
        .get(usize::from(opcode))
        .copied()
        .unwrap_or("?")
}

/// Error codes (the firmware's `Errors.h`; the library's `FERR_` codes use
/// the same numbers).
#[allow(dead_code)]
pub mod err {
    pub const OKAY: u8 = 0x00;
    pub const CMDFAILED: u8 = 0x01;
    pub const BUSY: u8 = 0x02;
    pub const ABORTED: u8 = 0x03;
    pub const BADPARAM: u8 = 0x04;
    pub const BADCOMMAND: u8 = 0x05;
    pub const WRONGMODE: u8 = 0x07;
    pub const EEFAILURE: u8 = 0x13;
    pub const CRASH: u8 = 0x1E;
    pub const OBJNOTFOUND: u8 = 0x20;
    pub const EXHAUSTED: u8 = 0x23;
    pub const SELTIMEOUT: u8 = 0x40;
}

/// One command slot, by number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Slot(pub usize);

impl Slot {
    /// SRAM offset of the slot's opcode word.
    pub fn base(self) -> usize {
        CMD_BASE + self.0 * CMD_LEN
    }

    pub fn opcode_word(self, sram: &[u8]) -> u16 {
        rd16(sram, self.base())
    }

    pub fn set_opcode_word(self, sram: &mut [u8], word: u16) {
        wr16(sram, self.base(), word);
    }

    /// The status bits of the opcode word.
    pub fn status(self, sram: &[u8]) -> u16 {
        self.opcode_word(sram) & STAT_MASK
    }

    /// The opcode under the status bits.
    pub fn opcode(self, sram: &[u8]) -> u16 {
        self.opcode_word(sram) & OPCODE_MASK
    }

    pub fn cont(self, sram: &[u8]) -> u8 {
        sram[self.base() + 2]
    }

    pub fn set_cont(self, sram: &mut [u8], cont: u8) {
        sram[self.base() + 2] = cont;
    }

    pub fn error(self, sram: &[u8]) -> u8 {
        sram[self.base() + 3]
    }

    pub fn set_error(self, sram: &mut [u8], error: u8) {
        sram[self.base() + 3] = error;
    }

    /// Mark the command in progress and clear its error byte, as the
    /// firmware's `CommandBusy` does before running it.
    pub fn mark_busy(self, sram: &mut [u8]) {
        let word = self.opcode_word(sram);
        self.set_opcode_word(sram, (word & OPCODE_MASK) | STAT_INPROG);
        self.set_error(sram, 0);
    }

    /// Mark the command done (`CommandDone`: OR in both status bits).
    pub fn mark_done(self, sram: &mut [u8]) {
        let word = self.opcode_word(sram);
        self.set_opcode_word(sram, word | STAT_DONE);
    }

    /// Argument byte at slot offset `off` (4-31).
    pub fn arg8(self, sram: &[u8], off: usize) -> u8 {
        sram[self.base() + off]
    }

    pub fn arg16(self, sram: &[u8], off: usize) -> u16 {
        rd16(sram, self.base() + off)
    }

    pub fn arg32(self, sram: &[u8], off: usize) -> u32 {
        rd32(sram, self.base() + off)
    }

    pub fn set_arg8(self, sram: &mut [u8], off: usize, value: u8) {
        sram[self.base() + off] = value;
    }

    pub fn set_arg16(self, sram: &mut [u8], off: usize, value: u16) {
        wr16(sram, self.base() + off, value);
    }

    pub fn set_arg32(self, sram: &mut [u8], off: usize, value: u32) {
        wr32(sram, self.base() + off, value);
    }
}

/// What a write of `len` bytes at window offset `off` did to one slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotTouch {
    pub slot: Slot,
    /// It wrote a byte of the opcode word.
    pub opcode: bool,
    /// It wrote the `cont` byte.
    pub cont: bool,
}

/// The slots a write of `len` bytes at `off` reached, in slot order.
pub fn touched_slots(off: usize, len: usize) -> impl Iterator<Item = SlotTouch> {
    let end = off.saturating_add(len);
    let cmd_end = CMD_BASE + CMD_SLOTS * CMD_LEN;
    let (first, last) = if end <= CMD_BASE || off >= cmd_end {
        (1, 0)
    } else {
        (
            off.max(CMD_BASE).saturating_sub(CMD_BASE) / CMD_LEN,
            (end.min(cmd_end) - 1 - CMD_BASE) / CMD_LEN,
        )
    };
    (first..=last).filter_map(move |n| {
        let base = CMD_BASE + n * CMD_LEN;
        let hits = |a: usize, b: usize| off < base + b && end > base + a;
        let touch = SlotTouch {
            slot: Slot(n),
            opcode: hits(0, 2),
            cont: hits(2, 3),
        };
        (touch.opcode || touch.cont).then_some(touch)
    })
}

/// Big-endian SRAM accessors. Offsets wrap at the SRAM size, as the
/// Flyer's 18 SRAM address lines do; a pointer the Amiga got wrong reads
/// and writes the wrong place instead of stopping the emulator.
pub fn rd8(sram: &[u8], off: usize) -> u8 {
    sram[off & (SRAM_SIZE - 1)]
}

pub fn rd16(sram: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([rd8(sram, off), rd8(sram, off + 1)])
}

pub fn rd32(sram: &[u8], off: usize) -> u32 {
    (u32::from(rd16(sram, off)) << 16) | u32::from(rd16(sram, off + 2))
}

pub fn wr8(sram: &mut [u8], off: usize, value: u8) {
    sram[off & (SRAM_SIZE - 1)] = value;
}

pub fn wr16(sram: &mut [u8], off: usize, value: u16) {
    let [hi, lo] = value.to_be_bytes();
    wr8(sram, off, hi);
    wr8(sram, off + 1, lo);
}

pub fn wr32(sram: &mut [u8], off: usize, value: u32) {
    wr16(sram, off, (value >> 16) as u16);
    wr16(sram, off + 2, value as u16);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_table_matches_the_library_numbering() {
        // Spot checks against opcodes.i (DEFCMD order from $0000).
        assert_eq!(opcode_name(op::FIRMWARE), "FIRMWARE");
        assert_eq!(opcode_name(op::GETCLOCK), "GETCLOCK");
        assert_eq!(opcode_name(op::DRIVEINFO), "DRIVEINFO");
        assert_eq!(opcode_name(op::FLOOBY), "FLOOBY");
        assert_eq!(opcode_name(op::QUIT), "QUIT");
        assert_eq!(opcode_name(op::EEREAD), "EEREAD");
        assert_eq!(opcode_name(op::TBC), "TBC");
        assert_eq!(opcode_name(op::CACHETEST), "CACHETEST");
        assert_eq!(opcode_name(op::FILEHSWRITE), "FILEHSWRITE");
        assert_eq!(op::COUNT as usize, OPCODE_NAMES.len());
        assert_eq!(opcode_name(op::COUNT), "?");
    }

    #[test]
    fn slot_status_follows_the_two_top_bits() {
        let mut sram = vec![0u8; SRAM_SIZE];
        let slot = Slot(3);
        assert_eq!(slot.base(), 0x560);
        slot.set_opcode_word(&mut sram, 0x0001);
        assert_eq!(slot.status(&sram), STAT_BLANK);
        slot.set_opcode_word(&mut sram, op::EEREAD | STAT_NEW);
        assert_eq!(slot.status(&sram), STAT_NEW);
        assert_eq!(slot.opcode(&sram), op::EEREAD);
        slot.set_error(&mut sram, 9);
        slot.mark_busy(&mut sram);
        assert_eq!(slot.status(&sram), STAT_INPROG);
        assert_eq!(slot.error(&sram), 0);
        slot.mark_done(&mut sram);
        assert_eq!(slot.opcode_word(&sram), op::EEREAD | STAT_DONE);
    }

    #[test]
    fn writes_report_the_slot_fields_they_reach() {
        // The opcode word of slot 0.
        let t: Vec<_> = touched_slots(0x500, 2).collect();
        assert_eq!(
            t,
            [SlotTouch {
                slot: Slot(0),
                opcode: true,
                cont: false
            }]
        );
        // Its low byte alone, and the cont byte alone.
        assert!(touched_slots(0x501, 1).next().unwrap().opcode);
        let cont = touched_slots(0x502, 1).next().unwrap();
        assert!(cont.cont && !cont.opcode);
        // A long write over opcode and cont.
        let both = touched_slots(0x520, 4).next().unwrap();
        assert_eq!(both.slot, Slot(1));
        assert!(both.opcode && both.cont);
        // Arguments and the error byte are not interesting.
        assert_eq!(touched_slots(0x503, 1).count(), 0);
        assert_eq!(touched_slots(0x504, 28).count(), 0);
        // Outside the mailbox.
        assert_eq!(touched_slots(0x4FE, 2).count(), 0);
        assert_eq!(touched_slots(0x600, 4).count(), 0);
        assert_eq!(touched_slots(0x1_8000, 4).count(), 0);
        // A write spanning the end of one slot and the start of the next.
        let t: Vec<_> = touched_slots(0x53E, 4).collect();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].slot, Slot(2));
        // The last slot.
        assert_eq!(touched_slots(0x5E0, 2).next().unwrap().slot, Slot(7));
    }

    #[test]
    fn accessors_are_big_endian_and_wrap_at_the_sram_size() {
        let mut sram = vec![0u8; SRAM_SIZE];
        wr32(&mut sram, 0x100, 0x1234_5678);
        assert_eq!(sram[0x100..0x104], [0x12, 0x34, 0x56, 0x78]);
        assert_eq!(rd16(&sram, 0x102), 0x5678);
        wr16(&mut sram, SRAM_SIZE - 1, 0xABCD);
        assert_eq!(sram[SRAM_SIZE - 1], 0xAB);
        assert_eq!(sram[0], 0xCD);
        assert_eq!(rd8(&sram, SRAM_SIZE + 0x100), 0x12);
    }
}
