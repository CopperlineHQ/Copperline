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
use m68k::{NoOpHleHandler, StepResult};

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
/// table. The stack/vector page, the code page, and the table page are
/// identity-mapped (so fault handlers can run with translation on); the
/// FAULT_PAGE page-table slot holds `page_desc`; everything else is
/// invalid. All accesses translate, exception frame pushes and vector
/// fetches included -- an invalid descriptor faults instruction fetches
/// exactly like data (demand-paged code).
fn user_mode_040_with_table(bus: &mut TestBus, page_desc: u32) -> CpuCore {
    // Root and pointer table entries: UDT resident (bits 1:0 >= 2).
    bus.write_long(ROOT_TABLE, PTR_TABLE | 2);
    bus.write_long(PTR_TABLE, PAGE_TABLE | 2);
    bus.write_long(PAGE_TABLE, 0x0000_0001); // page 0: vectors, code
    bus.write_long(PAGE_TABLE + 4, 0x0000_1001); // page 1: stacks
    bus.write_long(PAGE_TABLE + 8 * 4, 0x0000_8001); // page 8: the tables
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
const F_WB3S: u32 = 0x0E;
const F_FA: u32 = 0x14;
const F_WB3A: u32 = 0x18;
const F_WB3D: u32 = 0x1C;

fn frame_base(cpu: &CpuCore) -> u32 {
    let sp = cpu.a(7);
    assert_eq!(sp, SSP - FRAME_LEN, "format $7 frame is 30 words");
    sp
}

fn step_n(cpu: &mut CpuCore, bus: &mut TestBus, n: usize) {
    let mut hle = NoOpHleHandler;
    for _ in 0..n {
        let r = cpu.step_with_hle_handler(bus, &mut hle);
        assert!(
            matches!(r, StepResult::Ok { .. }),
            "unexpected step result {r:?} at pc={:#010X}",
            cpu.pc
        );
    }
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
    assert_eq!(
        bus.read_word(f + F_WB3S),
        0,
        "no writeback for a read fault"
    );
}

/// A faulted write is reported in writeback slot 3: WB3S carries the valid
/// bit, size, and transfer modifier, with the address and data in
/// WB3A/WB3D (matching real 68040 silicon and the WinUAE reference; WB2 is
/// reserved for MOVE16). This is the frame contract Enforcer/MuForce use to
/// report a hit's data and MuGuardianAngel uses to complete the store.
#[test]
fn write_fault_frame_carries_writeback_3() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_040_with_table(&mut bus, 0);

    cpu.set_d(0, 0x1234_5678);
    bus.write_word(CODE, 0x2080); // MOVE.L D0,(A0)

    let _ = cpu.step(&mut bus);
    let f = frame_base(&cpu);
    assert_eq!(bus.read_word(f + F_SSW), 0x0401, "SSW = ATC | write | long");
    assert_eq!(
        bus.read_word(f + F_WB3S),
        0x0081,
        "WB3S = V | SZ=long | TM=user data"
    );
    assert_eq!(bus.read_long(f + F_WB3A), FAULT_PAGE, "writeback address");
    assert_eq!(bus.read_long(f + F_WB3D), 0x1234_5678, "writeback data");
}

/// A handler that clears WB3S.V has absorbed the faulted write (an
/// Enforcer/MuForce hit on a protected page): the restarted instruction
/// continues past the store without re-faulting, and the page stays
/// invalid.
#[test]
fn rte_with_wb3s_cleared_absorbs_the_faulted_write() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_040_with_table(&mut bus, 0);

    cpu.set_d(0, 0x1234_5678);
    bus.write_word(CODE, 0x2080); // MOVE.L D0,(A0)
    bus.write_word(CODE + 2, 0x4E71); // NOP

    // Handler: CLR.B ($0F,A7) (WB3S low byte, V included); RTE.
    bus.write_word(HANDLER, 0x422F);
    bus.write_word(HANDLER + 2, (F_WB3S + 1) as u16);
    bus.write_word(HANDLER + 4, 0x4E73);

    // Fault + handler (2) + suppressed rerun + NOP.
    step_n(&mut cpu, &mut bus, 5);
    assert!(!cpu.is_supervisor(), "no re-fault loop");
    assert_eq!(cpu.pc & 0xFFFF, (CODE + 4) & 0xFFFF, "past the NOP");
}

/// A handler that fixes the mapping and leaves WB3S.V set gets the plain
/// restart: the re-executed store lands through the new translation.
#[test]
fn rte_with_wb3s_valid_restarts_the_write() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_040_with_table(&mut bus, 0);

    cpu.set_d(0, 0x1234_5678);
    bus.write_word(CODE, 0x2080); // MOVE.L D0,(A0)
    bus.write_word(CODE + 2, 0x4E71); // NOP

    // Handler: materialize the page (FAULT_PAGE -> physical 0x6000), RTE.
    // MOVE.L #$00006001,(page table slot).L
    bus.write_word(HANDLER, 0x23FC);
    bus.write_long(HANDLER + 2, 0x0000_6001);
    bus.write_long(HANDLER + 6, PAGE_TABLE + (FAULT_PAGE >> 12) * 4);
    bus.write_word(HANDLER + 10, 0x4E73);

    step_n(&mut cpu, &mut bus, 5);
    assert!(!cpu.is_supervisor());
    assert_eq!(
        bus.read_long(0x6000),
        0x1234_5678,
        "restarted write landed via the new mapping"
    );
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

/// A JSR whose stack push faults must end up in the vector-2 handler, not
/// at its branch target: the aborted instruction's flow change may not
/// survive the dispatch (Linux/m68k grows user stacks by faulting exactly
/// this push; diverting into the target skips the kernel entirely).
#[test]
fn jsr_whose_stack_push_faults_enters_the_handler() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_040_with_table(&mut bus, 0);

    cpu.set_a(7, FAULT_PAGE + 0x20); // user SP on the invalid page
    bus.write_word(CODE, 0x4EB9); // JSR ($00000400).L
    bus.write_long(CODE + 2, 0x0000_0400);

    let _ = cpu.step(&mut bus);
    assert!(cpu.is_supervisor(), "push fault must enter supervisor mode");
    assert_eq!(cpu.pc, HANDLER, "dispatch wins over the JSR target");

    let f = frame_base(&cpu);
    assert_eq!(bus.read_word(f + F_FMT), 0x7008, "format $7, vector 2");
    assert_eq!(bus.read_long(f + F_PC), CODE, "restart PC is the JSR");
}

/// A predecrement MOVEM whose first store faults must leave the handler on
/// an intact supervisor stack: the aborted instruction may not keep
/// stepping A7 underneath the freshly-pushed frame (the handler would run
/// on a stale, user-derived stack pointer).
#[test]
fn movem_predecrement_fault_leaves_the_dispatch_stack_intact() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_040_with_table(&mut bus, 0);

    cpu.set_a(7, FAULT_PAGE + 0x40); // user SP on the invalid page
    bus.write_word(CODE, 0x48E7); // MOVEM.L D0-D7/A0-A6,-(A7)
    bus.write_word(CODE + 2, 0xFFFE);

    let _ = cpu.step(&mut bus);
    assert!(
        cpu.is_supervisor(),
        "store fault must enter supervisor mode"
    );
    assert_eq!(cpu.pc, HANDLER, "dispatch reaches the handler");

    // frame_base itself asserts A7 == SSP - FRAME_LEN: no residual
    // decrements from the aborted MOVEM.
    let f = frame_base(&cpu);
    assert_eq!(bus.read_long(f + F_PC), CODE, "restart PC is the MOVEM");
}

/// A faulted MOVES data cycle translates in the SFC/DFC space, but the
/// dispatch it triggers does not: the frame pushes and the vector fetch
/// run in supervisor space. With split user/supervisor root pointers
/// (Linux/m68k), a leaked override would walk the user table for the
/// vector and read garbage where the handler address should be.
#[test]
fn moves_data_fault_vectors_in_supervisor_space() {
    const U_ROOT: u32 = 0x9000;
    const U_PTR: u32 = 0x9200;

    let mut bus = TestBus::new(0x10000);

    // Supervisor table: identity map for vectors/code/stack/tables.
    bus.write_long(ROOT_TABLE, PTR_TABLE | 2);
    bus.write_long(PTR_TABLE, PAGE_TABLE | 2);
    bus.write_long(PAGE_TABLE, 0x0000_0001);
    bus.write_long(PAGE_TABLE + 4, 0x0000_1001);
    bus.write_long(PAGE_TABLE + 8 * 4, 0x0000_8001);
    // User table: every page invalid (the U_PAGE table is all zeros).
    bus.write_long(U_ROOT, U_PTR | 2);
    bus.write_long(U_PTR, (U_PTR + 0x100) | 2);

    bus.write_long(8, HANDLER);
    bus.write_word(HANDLER, 0x4E73);

    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);
    cpu.mmu_crp_aptr = U_ROOT; // URP: user space, nothing mapped
    cpu.mmu_srp_aptr = ROOT_TABLE; // SRP: kernel space, vectors live here
    cpu.mmu_tc = 0x0000_8000;
    cpu.pmmu_enabled = true;
    cpu.set_sr(0x2700);
    cpu.set_a(7, SSP);
    cpu.set_a(0, FAULT_PAGE);
    cpu.set_d(0, 0xDEAD_BEEF);
    cpu.set_d(1, 1); // DFC = 1: user data space
    cpu.pc = CODE;

    bus.write_word(CODE, 0x4E7B); // MOVEC D1,DFC
    bus.write_word(CODE + 2, 0x1001);
    bus.write_word(CODE + 4, 0x0E90); // MOVES.L D0,(A0)
    bus.write_word(CODE + 6, 0x0800);

    let _ = cpu.step(&mut bus); // MOVEC
    let _ = cpu.step(&mut bus); // MOVES faults in the user space
    assert_eq!(cpu.stopped, 0, "no double fault: the vector fetch is fine");
    assert_eq!(cpu.pc, HANDLER, "vector read through SRP");

    let sp = cpu.a(7);
    assert_eq!(sp, SSP - FRAME_LEN, "frame pushed on the supervisor stack");
    assert_eq!(bus.read_word(sp + F_FMT), 0x7008, "format $7, vector 2");
    assert_eq!(
        bus.read_word(sp + F_SSW),
        0x0401,
        "SSW = ATC | write | SZ=long | TM=1: the MOVES DFC space"
    );
    assert_eq!(
        bus.read_long(sp + F_WB3D),
        0xDEAD_BEEF,
        "writeback data is the faulted store's value"
    );
}
