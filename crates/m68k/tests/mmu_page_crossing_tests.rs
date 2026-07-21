//! Misaligned accesses that straddle an MMU page boundary.
//!
//! The 32-bit-bus CPUs run such an access as separate bus cycles, and the
//! MMU translates each cycle on its own page: the two halves may live on
//! virtual pages that map to unrelated physical pages. Translating only
//! the base address and issuing one flat physical access reads or writes
//! the physically adjacent page instead of the mapped one.
//!
//! The instruction-stream variant of this broke Debian/m68k userspace: a
//! BSR.L whose 32-bit displacement straddled a page fetched its low half
//! from an unrelated physical page, jumped into the middle of an
//! instruction, and the accidental store corrupted bash's heap.

use m68k::core::cpu::CpuCore;
use m68k::core::memory::{AddressBus, BusFault};
use m68k::core::types::CpuType;

/// Flat test bus backed by a byte vector.
struct TestBus {
    mem: Vec<u8>,
}

impl TestBus {
    fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    fn write_long(&mut self, addr: u32, val: u32) {
        for i in 0..4 {
            self.mem[addr as usize + i] = (val >> (24 - 8 * i)) as u8;
        }
    }
}

impl AddressBus for TestBus {
    fn read_byte(&mut self, addr: u32) -> u8 {
        self.mem.get(addr as usize).copied().unwrap_or(0)
    }

    fn write_byte(&mut self, addr: u32, val: u8) {
        if let Some(m) = self.mem.get_mut(addr as usize) {
            *m = val;
        }
    }

    fn read_word(&mut self, addr: u32) -> u16 {
        ((self.read_byte(addr) as u16) << 8) | self.read_byte(addr + 1) as u16
    }

    fn write_word(&mut self, addr: u32, val: u16) {
        self.write_byte(addr, (val >> 8) as u8);
        self.write_byte(addr + 1, val as u8);
    }

    fn read_long(&mut self, addr: u32) -> u32 {
        ((self.read_word(addr) as u32) << 16) | self.read_word(addr + 2) as u32
    }

    fn write_long(&mut self, addr: u32, val: u32) {
        self.write_word(addr, (val >> 16) as u16);
        self.write_word(addr + 2, val as u16);
    }

    fn try_read_byte(&mut self, addr: u32) -> Result<u8, BusFault> {
        Ok(self.read_byte(addr))
    }

    fn try_read_word(&mut self, addr: u32) -> Result<u16, BusFault> {
        Ok(self.read_word(addr))
    }

    fn try_read_long(&mut self, addr: u32) -> Result<u32, BusFault> {
        Ok(self.read_long(addr))
    }
}

const SSP: u32 = 0x1F00;
const USP: u32 = 0x1800;
const CODE: u32 = 0x0100;
const ROOT_TABLE: u32 = 0x8000;
const PTR_TABLE: u32 = 0x8200;
const PAGE_TABLE: u32 = 0x8400;

/// Logical pages 5 and 6 are adjacent, but their physical pages are not:
/// 0x5000 -> 0x6000 and 0x6000 -> 0xA000. An access spanning the 0x6000
/// logical boundary must split across physical 0x6FFF / 0xA000; a flat
/// base-translated access would spill into physical 0x7000 instead.
const LO_LOG: u32 = 0x5000;
const LO_PHYS: u32 = 0x6000;
const HI_PHYS: u32 = 0xA000;

fn user_mode_040_two_pages(bus: &mut TestBus) -> CpuCore {
    bus.write_long(ROOT_TABLE, PTR_TABLE | 2);
    bus.write_long(PTR_TABLE, PAGE_TABLE | 2);
    bus.write_long(PAGE_TABLE, 0x0000_0001); // page 0: vectors, code
    bus.write_long(PAGE_TABLE + 4, 0x0000_1001); // page 1: stacks
    bus.write_long(PAGE_TABLE + 8 * 4, 0x0000_8001); // page 8: the tables
    bus.write_long(PAGE_TABLE + 5 * 4, LO_PHYS | 1);
    bus.write_long(PAGE_TABLE + 6 * 4, HI_PHYS | 1);

    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);
    cpu.mmu_crp_aptr = ROOT_TABLE;
    cpu.mmu_srp_aptr = ROOT_TABLE;
    cpu.mmu_tc = 0x0000_8000; // E=1, 4K pages
    cpu.pmmu_enabled = true;

    cpu.set_sr(0x2700);
    cpu.set_a(7, SSP);
    cpu.set_sr(0x0000);
    cpu.set_a(7, USP);
    cpu.pc = CODE;
    cpu
}

fn step_ok(cpu: &mut CpuCore, bus: &mut TestBus) {
    let r = cpu.step(bus);
    assert!(
        matches!(r, m68k::StepResult::Ok { .. }),
        "unexpected step result {r:?} at pc={:#010X}",
        cpu.pc
    );
    assert!(!cpu.is_supervisor(), "access must not fault");
}

#[test]
fn page_crossing_long_read_translates_each_half() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_040_two_pages(&mut bus);

    bus.write_word(LO_PHYS + 0xFFE, 0xDEAD);
    bus.write_word(HI_PHYS, 0xBEEF);
    bus.write_word(LO_PHYS + 0x1000, 0x7777); // physically adjacent decoy

    cpu.set_a(0, LO_LOG + 0xFFE);
    bus.write_word(CODE, 0x2010); // MOVE.L (A0),D0

    step_ok(&mut cpu, &mut bus);
    assert_eq!(
        cpu.d(0),
        0xDEAD_BEEF,
        "low half must come from the mapped page, not the physically adjacent one"
    );
}

#[test]
fn page_crossing_long_write_translates_each_half() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_040_two_pages(&mut bus);

    cpu.set_a(0, LO_LOG + 0xFFE);
    cpu.set_d(0, 0xCAFE_F00D);
    bus.write_word(CODE, 0x2080); // MOVE.L D0,(A0)

    step_ok(&mut cpu, &mut bus);
    assert_eq!(bus.read_word(LO_PHYS + 0xFFE), 0xCAFE, "high half");
    assert_eq!(
        bus.read_word(HI_PHYS),
        0xF00D,
        "low half on the mapped page"
    );
    assert_eq!(
        bus.read_word(LO_PHYS + 0x1000),
        0,
        "nothing may spill onto the physically adjacent page"
    );
}

#[test]
fn page_crossing_word_write_translates_each_byte() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_040_two_pages(&mut bus);

    cpu.set_a(0, LO_LOG + 0xFFF);
    cpu.set_d(0, 0x0000_A55A);
    bus.write_word(CODE, 0x3080); // MOVE.W D0,(A0)

    step_ok(&mut cpu, &mut bus);
    assert_eq!(bus.read_byte(LO_PHYS + 0xFFF), 0xA5, "high byte");
    assert_eq!(bus.read_byte(HI_PHYS), 0x5A, "low byte on the mapped page");
    assert_eq!(bus.read_byte(LO_PHYS + 0x1000), 0, "no spill");
}

#[test]
fn page_crossing_rmw_long_composes_and_stores_through_both_pages() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_040_two_pages(&mut bus);

    bus.write_word(LO_PHYS + 0xFFE, 0x0001);
    bus.write_word(HI_PHYS, 0x0134);

    cpu.set_a(0, LO_LOG + 0xFFE);
    cpu.set_d(0, 0x1111_0000);
    bus.write_word(CODE, 0xD190); // ADD.L D0,(A0)

    step_ok(&mut cpu, &mut bus);
    assert_eq!(
        bus.read_word(LO_PHYS + 0xFFE),
        0x1112,
        "high half of the sum"
    );
    assert_eq!(bus.read_word(HI_PHYS), 0x0134, "low half of the sum");
}

/// The Debian/m68k regression: a BSR.L whose 32-bit displacement extension
/// straddles the page boundary must fetch each extension word through its
/// own translation. With a flat base-translated fetch the displacement's
/// low half came from the physically adjacent page and the call landed in
/// the middle of an unrelated instruction.
#[test]
fn page_crossing_bsr_long_displacement_fetches_each_word_translated() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_040_two_pages(&mut bus);

    // BSR.L at logical 0x5FFC: opcode + displacement high word sit at the
    // end of the LO page, the displacement low word at the start of the HI
    // page. disp = 0x00000010 -> target 0x5FFE + 0x10 = 0x600E.
    bus.write_word(LO_PHYS + 0xFFC, 0x61FF);
    bus.write_word(LO_PHYS + 0xFFE, 0x0000);
    bus.write_word(HI_PHYS, 0x0010);
    bus.write_word(HI_PHYS + 0xE, 0x4E71); // NOP at the branch target
    bus.write_word(LO_PHYS + 0x1000, 0x7FF0); // decoy displacement low word

    cpu.pc = LO_LOG + 0xFFC;
    step_ok(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x600E, "branch target uses the mapped low word");
    assert_eq!(
        bus.read_long(USP - 4),
        0x6002,
        "return address pushed past the whole 6-byte instruction"
    );
}
