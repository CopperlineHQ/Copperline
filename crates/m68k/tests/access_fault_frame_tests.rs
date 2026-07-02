//! 68040 access-error (format $7) stack frame contents.
//!
//! An OS-level page-fault handler (mmu.library, VMM, Enforcer) decides from
//! the frame's special status word whether a fault is an MMU translation
//! fault it must service (SSW ATC bit set) or a physical bus error it must
//! pass on to the OS (ATC clear). mmu.library additionally builds its user
//! table out of indirect page descriptors whose shared targets start out
//! poisoned and are materialized lazily from the vector-2 handler, so a
//! translation fault that is not reported as an ATC fault gurus the machine
//! (issue #90: SetPatch + MMU libs crash in ramlib with #80000002).

use m68k::core::cpu::CpuCore;
use m68k::core::memory::{AddressBus, BusFault, BusFaultKind};
use m68k::core::types::CpuType;
use m68k::StepResult;

/// Simple test bus that stores memory in a vector.
struct TestBus {
    mem: Vec<u8>,
    /// Longword-aligned start of a region whose accesses raise a physical
    /// bus error (None = whole bus is well-behaved RAM).
    fault_at: Option<u32>,
}

impl TestBus {
    fn new(size: usize) -> Self {
        Self {
            mem: vec![0; size],
            fault_at: None,
        }
    }

    fn write_long(&mut self, addr: u32, val: u32) {
        let addr = addr as usize;
        if addr + 3 < self.mem.len() {
            self.mem[addr] = (val >> 24) as u8;
            self.mem[addr + 1] = (val >> 16) as u8;
            self.mem[addr + 2] = (val >> 8) as u8;
            self.mem[addr + 3] = val as u8;
        }
    }

    fn faults(&self, addr: u32) -> bool {
        self.fault_at
            .is_some_and(|base| (base..base + 0x1000).contains(&addr))
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
        let hi = self.read_byte(addr) as u16;
        let lo = self.read_byte(addr + 1) as u16;
        (hi << 8) | lo
    }

    fn write_word(&mut self, addr: u32, val: u16) {
        self.write_byte(addr, (val >> 8) as u8);
        self.write_byte(addr + 1, val as u8);
    }

    fn read_long(&mut self, addr: u32) -> u32 {
        let hi = self.read_word(addr) as u32;
        let lo = self.read_word(addr + 2) as u32;
        (hi << 16) | lo
    }

    fn write_long(&mut self, addr: u32, val: u32) {
        self.write_word(addr, (val >> 16) as u16);
        self.write_word(addr + 2, val as u16);
    }

    fn try_read_byte(&mut self, addr: u32) -> Result<u8, BusFault> {
        if self.faults(addr) {
            return Err(BusFault {
                kind: BusFaultKind::BusError,
                address: addr,
            });
        }
        Ok(self.read_byte(addr))
    }

    fn try_read_word(&mut self, addr: u32) -> Result<u16, BusFault> {
        if self.faults(addr) {
            return Err(BusFault {
                kind: BusFaultKind::BusError,
                address: addr,
            });
        }
        Ok(self.read_word(addr))
    }

    fn try_read_long(&mut self, addr: u32) -> Result<u32, BusFault> {
        if self.faults(addr) {
            return Err(BusFault {
                kind: BusFaultKind::BusError,
                address: addr,
            });
        }
        Ok(self.read_long(addr))
    }
}

const SSP: u32 = 0x1F00;
const USP: u32 = 0x1800;
const CODE: u32 = 0x0100;
const HANDLER: u32 = 0x0300;
const ROOT_TABLE: u32 = 0x8000;
const PTR_TABLE: u32 = 0x8200;
const PAGE_TABLE: u32 = 0x8400;
/// Logical test page (ri=0, pi=0, pgi=5 with 4K pages).
const FAULT_PAGE: u32 = 0x5000;

/// A 68040 in user mode with translation enabled through a three-level user
/// table. Every entry is invalid except the FAULT_PAGE page-table slot,
/// which holds `page_desc`. Code/stack pages walk into invalid descriptors
/// and use the instruction-fetch identity fallback; the exception frame
/// itself is pushed with translation bypassed (exception_processing), so
/// only explicit data accesses exercise the walker.
fn user_mode_040_with_table(bus: &mut TestBus, page_desc: u32) -> CpuCore {
    // Root and pointer table entries: UDT resident (bits 1:0 >= 2).
    bus.write_long(ROOT_TABLE, PTR_TABLE | 2);
    bus.write_long(PTR_TABLE, PAGE_TABLE | 2);
    bus.write_long(PAGE_TABLE + (FAULT_PAGE >> 12) * 4, page_desc);

    // Vector 2 (bus error) -> HANDLER, which is a bare RTE.
    bus.write_long(8, HANDLER);
    bus.write_word(HANDLER as u16 as u32, 0x4E73);

    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);
    cpu.mmu_crp_aptr = ROOT_TABLE; // URP (user root)
    cpu.mmu_srp_aptr = ROOT_TABLE;
    cpu.mmu_tc = 0x0000_8000; // E=1, 4K pages
    cpu.pmmu_enabled = true;

    // Bank SSP, then drop to user mode with its own stack.
    cpu.set_sr(0x2700);
    cpu.set_a(7, SSP);
    cpu.set_sr(0x0000);
    cpu.set_a(7, USP);
    cpu.set_a(0, FAULT_PAGE);
    cpu.pc = CODE;
    cpu
}

/// Frame field offsets from the post-exception supervisor SP (M68040UM 8.4.3).
const FRAME_LEN: u32 = 0x3C;
const F_SR: u32 = 0x00;
const F_PC: u32 = 0x02;
const F_FMT: u32 = 0x06;
const F_EA: u32 = 0x08;
const F_SSW: u32 = 0x0C;
const F_FA: u32 = 0x14;

fn frame_base(cpu: &CpuCore) -> u32 {
    let sp = cpu.a(7);
    assert_eq!(sp, SSP - FRAME_LEN, "format $7 frame is 30 words");
    sp
}

/// A user-mode data read through an invalid page descriptor pushes a format
/// $7 frame whose SSW reports an ATC fault (bit 10), a read (bit 8), the
/// long size (bits 6:5 = 00) and the user-data transfer modifier (TM=1),
/// with the fault address in FA and the restart PC on the faulting
/// instruction.
#[test]
fn translation_fault_frame_reports_atc_read_long() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_040_with_table(&mut bus, 0); // PDT invalid

    bus.write_word(CODE, 0x2010); // MOVE.L (A0),D0

    let result = cpu.step(&mut bus);
    assert!(matches!(result, StepResult::Ok { .. }));
    assert!(cpu.is_supervisor(), "fault must enter supervisor mode");
    assert_eq!(cpu.pc & 0xFFFF, HANDLER, "must vector through vector 2");

    let f = frame_base(&cpu);
    assert_eq!(bus.read_word(f + F_FMT), 0x7008, "format $7, vector 2");
    assert_eq!(
        bus.read_word(f + F_SSW),
        0x0501,
        "SSW = ATC | RW=read | SZ=long | TM=user data"
    );
    assert_eq!(bus.read_long(f + F_FA), FAULT_PAGE, "fault address");
    assert_eq!(bus.read_long(f + F_EA), FAULT_PAGE, "effective address");
    assert_eq!(bus.read_long(f + F_PC), CODE, "restart PC");
    assert_eq!(bus.read_word(f + F_SR) & 0x2000, 0, "stacked SR is user");
}

/// SSW size field follows the 68040 encoding: byte=01, word=10, long=00
/// (bits 6:5); a write clears the RW bit.
#[test]
fn translation_fault_ssw_size_and_direction_encodings() {
    for (opcode, want_ssw, what) in [
        (0x1010u16, 0x0521u16, "MOVE.B (A0),D0: byte read"),
        (0x3010, 0x0541, "MOVE.W (A0),D0: word read"),
        (0x2080, 0x0401, "MOVE.L D0,(A0): long write"),
    ] {
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_040_with_table(&mut bus, 0);
        bus.write_word(CODE, opcode);

        let _ = cpu.step(&mut bus);
        let f = frame_base(&cpu);
        assert_eq!(bus.read_word(f + F_SSW), want_ssw, "{what}");
    }
}

/// An indirect page descriptor (PDT=2) is followed to its target; a
/// resident target maps the page. mmu.library builds its whole user tree
/// this way (shared descriptors between the user and supervisor tables).
#[test]
fn indirect_page_descriptor_resolves_to_resident_target() {
    let mut bus = TestBus::new(0x10000);
    // Indirect descriptor -> shared descriptor at 0x9000 -> resident page
    // at physical 0x6000.
    let mut cpu = user_mode_040_with_table(&mut bus, 0x9000 | 2);
    bus.write_long(0x9000, 0x6000 | 1);
    bus.write_long(0x6000, 0xCAFE_F00D);

    bus.write_word(CODE, 0x2010); // MOVE.L (A0),D0

    let result = cpu.step(&mut bus);
    assert!(matches!(result, StepResult::Ok { .. }));
    assert!(!cpu.is_supervisor(), "resident mapping must not fault");
    assert_eq!(cpu.d(0), 0xCAFE_F00D, "read translates 0x5000 -> 0x6000");
}

/// An indirect descriptor whose target is still invalid faults as an ATC
/// fault. This is the exact issue #90 shape: mmu.library points user pages
/// at poisoned shared descriptors and materializes them from its vector-2
/// handler, which only claims the fault if SSW.ATC is set.
#[test]
fn indirect_page_descriptor_with_invalid_target_faults_as_atc() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_040_with_table(&mut bus, 0x9000 | 2);
    bus.write_long(0x9000, 0xBADF_EED0); // mmu.library's poison, PDT=00

    bus.write_word(CODE, 0x2010); // MOVE.L (A0),D0

    let _ = cpu.step(&mut bus);
    assert!(cpu.is_supervisor(), "invalid target must fault");
    let f = frame_base(&cpu);
    assert_eq!(bus.read_word(f + F_FMT), 0x7008);
    assert_eq!(bus.read_word(f + F_SSW), 0x0501, "reported as an ATC fault");
    assert_eq!(bus.read_long(f + F_FA), FAULT_PAGE);
}

/// A physical bus error (no MMU involvement) keeps the SSW ATC bit clear,
/// so a page-fault handler passes it on to the OS instead of treating it
/// as a translation fault.
#[test]
fn physical_bus_error_keeps_atc_clear() {
    let mut bus = TestBus::new(0x10000);
    bus.fault_at = Some(0x5000);

    // Vector 2 -> HANDLER.
    bus.write_long(8, HANDLER);
    bus.write_word(HANDLER, 0x4E73);
    bus.write_word(CODE, 0x2010); // MOVE.L (A0),D0

    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);
    cpu.set_sr(0x2700);
    cpu.set_a(7, SSP);
    cpu.set_sr(0x0000);
    cpu.set_a(7, USP);
    cpu.set_a(0, 0x5000);
    cpu.pc = CODE;

    let _ = cpu.step(&mut bus);
    assert!(cpu.is_supervisor(), "bus error must fault");
    let f = frame_base(&cpu);
    assert_eq!(bus.read_word(f + F_FMT), 0x7008);
    assert_eq!(
        bus.read_word(f + F_SSW),
        0x0101,
        "SSW = RW=read | SZ=long | TM=user data, ATC clear"
    );
}
