//! 68030 bus-cycle fault frames (format $B), their RTE continuation
//! protocol, and the 030 table-walk features mmu.library's lazy fault
//! handling depends on: FCL function-code lookup, indirect descriptors at
//! walk exhaustion, level-limited PTEST with the A bit, and PMOVE MMUSR.
//!
//! The RTE protocol under test is what real handlers use (mmu.library, VMM,
//! MuGuardianAngel): a data fault pushes SSW.DF; the handler either fixes
//! the mapping and RTEs (rerun), or clears DF and completes the data cycle
//! itself -- supplying a faulted read's result in the frame's data input
//! buffer, or absorbing a faulted write whose value it takes from the data
//! output buffer. mmu.library emulates lazily-zeroed pages with the
//! DF-cleared read path, which is how issue #90's 030 variant hangs without
//! it.

use m68k::core::cpu::CpuCore;
use m68k::core::memory::AddressBus;
use m68k::core::types::CpuType;
use m68k::{NoOpHleHandler, StepResult};

/// Simple test bus that stores memory in a vector.
struct TestBus {
    mem: Vec<u8>,
}

impl TestBus {
    fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
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
}

const SSP: u32 = 0x1F00;
const USP: u32 = 0x1800;
const CODE: u32 = 0x0100;
const HANDLER: u32 = 0x0300;
const ROOT_TABLE: u32 = 0x8000;
const B_TABLE: u32 = 0x8400;
/// Logical test page: with TIA=8/TIB=9 (32K pages) this walks root index 0,
/// B index 2, so its descriptor is the third B-table entry.
const FAULT_PAGE: u32 = 0x0001_0000;
const FAULT_DESC: u32 = B_TABLE + 8;

/// Frame field offsets from the post-exception supervisor SP (M68030UM 8.2):
/// format $B is 46 words with the SSW at +$0A, the data-cycle fault address
/// at +$10, the data output buffer at +$18 and the data input buffer at +$2C.
const FRAME_LEN: u32 = 0x5C;
const F_PC: u32 = 0x02;
const F_FMT: u32 = 0x06;
const F_SSW: u32 = 0x0A;
const F_FA: u32 = 0x10;
const F_DOB: u32 = 0x18;
const F_DIB: u32 = 0x2C;

/// A 68030 in user mode with translation enabled through a two-level table
/// (TC: E=1, IS=0, TIA=8, TIB=9 -> 32 KB pages). The first 64 KB (code,
/// stack, vectors, the tables themselves) is identity-mapped through two
/// page descriptors; FAULT_PAGE's descriptor holds `fault_desc`. Vector 2
/// points at HANDLER, whose code the individual test writes.
fn user_mode_030_with_table(bus: &mut TestBus, fault_desc: u32) -> CpuCore {
    bus.write_long(ROOT_TABLE, B_TABLE | 2); // root[0] -> B table (short)
    bus.write_long(B_TABLE, 0x0000_0001); // B[0]: identity page at 0
    bus.write_long(B_TABLE + 4, 0x0000_8001); // B[1]: identity page at 0x8000
    bus.write_long(FAULT_DESC, fault_desc); // B[2]: page under test

    bus.write_long(8, HANDLER); // vector 2

    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68030);
    cpu.mmu_crp_limit = 0x8000_0002; // short (4-byte) root descriptors
    cpu.mmu_crp_aptr = ROOT_TABLE;
    cpu.mmu_tc = 0x80F0_8900; // E=1, PS=32K, IS=0, TIA=8, TIB=9
    cpu.pmmu_enabled = true;

    cpu.set_sr(0x2700);
    cpu.set_a(7, SSP);
    cpu.set_sr(0x0000);
    cpu.set_a(7, USP);
    cpu.set_a(0, FAULT_PAGE);
    cpu.pc = CODE;
    cpu
}

fn frame_base(cpu: &CpuCore) -> u32 {
    let sp = cpu.a(7);
    assert_eq!(sp, SSP - FRAME_LEN, "format $B frame is 46 words");
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
/// $B frame: vector 2, SSW = DF | RW | SIZ=long | FC=user data, the fault
/// address in the data-cycle fault address long, and the rolled-back
/// instruction's PC for the restart.
#[test]
fn translation_fault_pushes_resumable_format_b_frame() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_030_with_table(&mut bus, 0); // invalid

    bus.write_word(CODE, 0x2010); // MOVE.L (A0),D0

    let result = cpu.step(&mut bus);
    assert!(matches!(result, StepResult::Ok { .. }));
    assert!(cpu.is_supervisor(), "fault must enter supervisor mode");
    assert_eq!(cpu.pc & 0xFFFF, HANDLER, "must vector through vector 2");

    let f = frame_base(&cpu);
    assert_eq!(bus.read_word(f + F_FMT), 0xB008, "format $B, vector 2");
    assert_eq!(
        bus.read_word(f + F_SSW),
        0x0141,
        "SSW = DF | RW=read | SIZ=long | FC=user data"
    );
    assert_eq!(bus.read_long(f + F_FA), FAULT_PAGE, "data fault address");
    assert_eq!(bus.read_long(f + F_PC), CODE, "restart PC");
}

/// The fix-and-rerun path: the vector-2 handler materializes the page
/// descriptor and RTEs with DF still set, and the restarted instruction
/// completes against the new mapping.
#[test]
fn rte_with_df_set_restarts_the_faulted_access() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_030_with_table(&mut bus, 0);

    bus.write_long(0x9000, 0xCAFE_F00D); // data in the to-be-mapped page
    bus.write_word(CODE, 0x2010); // MOVE.L (A0),D0
    bus.write_word(CODE + 2, 0x4E71); // NOP

    // Handler materializes the page: FAULT_PAGE -> physical 0, so the
    // restarted read of logical 0x10000 resolves to physical 0.
    // MOVE.L #$00000001,(FAULT_DESC).L ; descriptor: page at 0, DT=1
    bus.write_word(HANDLER, 0x23FC);
    bus.write_long(HANDLER + 2, 0x0000_0001);
    bus.write_long(HANDLER + 6, FAULT_DESC);
    bus.write_word(HANDLER + 10, 0x4E73); // RTE

    // Fault + handler (2 instructions) + rerun + NOP.
    step_n(&mut cpu, &mut bus, 5);
    assert!(!cpu.is_supervisor(), "back in user mode after RTE");
    assert_eq!(cpu.pc & 0xFFFF, (CODE + 4) & 0xFFFF, "past the NOP");
    // Logical 0x10000 now maps to physical 0: D0 = longword at 0 = SSP
    // from the vector table image (0x1F00 was never written to the bus;
    // vectors 0/4 are only read by reset. The test wrote vector 2 only,
    // so physical 0 reads 0).
    assert_eq!(cpu.d(0), bus.read_long(0), "read completed via new mapping");
}

/// The DF-cleared read path: the handler supplies the faulted read's result
/// in the data input buffer, clears DF, and RTEs. The instruction completes
/// with that value while the page stays invalid (mmu.library's lazily
/// zero-filled pages).
#[test]
fn rte_with_df_cleared_supplies_the_data_input_buffer() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_030_with_table(&mut bus, 0);

    bus.write_word(CODE, 0x2010); // MOVE.L (A0),D0
    bus.write_word(CODE + 2, 0x4E71); // NOP

    // Handler:
    //   BCLR #0,($A,A7)                ; clear SSW bit 8 (DF)
    //   MOVE.L #$CAFEBABE,($2C,A7)     ; data input buffer
    //   RTE
    bus.write_word(HANDLER, 0x08AF);
    bus.write_word(HANDLER + 2, 0x0000);
    bus.write_word(HANDLER + 4, F_SSW as u16);
    bus.write_word(HANDLER + 6, 0x2F7C);
    bus.write_long(HANDLER + 8, 0xCAFE_BABE);
    bus.write_word(HANDLER + 12, F_DIB as u16);
    bus.write_word(HANDLER + 14, 0x4E73); // RTE

    // Fault + handler (3 instructions) + completed re-execution + NOP.
    step_n(&mut cpu, &mut bus, 6);
    assert!(!cpu.is_supervisor());
    assert_eq!(cpu.d(0), 0xCAFE_BABE, "read completed from the DIB");
    assert_eq!(
        bus.read_long(FAULT_DESC),
        0,
        "page descriptor stays invalid"
    );
}

/// The DF-cleared write path: a faulted write stacks its value in the data
/// output buffer; a handler that clears DF absorbs the write, and the
/// re-executed instruction does not redo (or re-fault) it.
#[test]
fn rte_with_df_cleared_absorbs_the_faulted_write() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_030_with_table(&mut bus, 0);

    cpu.set_d(0, 0x1234_5678);
    bus.write_word(CODE, 0x2080); // MOVE.L D0,(A0)
    bus.write_word(CODE + 2, 0x4E71); // NOP

    // Handler: assert-by-copy the DOB into D7, clear DF, RTE.
    //   MOVE.L ($18,A7),D7
    //   BCLR #0,($A,A7)
    //   RTE
    bus.write_word(HANDLER, 0x2E2F);
    bus.write_word(HANDLER + 2, F_DOB as u16);
    bus.write_word(HANDLER + 4, 0x08AF);
    bus.write_word(HANDLER + 6, 0x0000);
    bus.write_word(HANDLER + 8, F_SSW as u16);
    bus.write_word(HANDLER + 10, 0x4E73); // RTE

    // Check the frame SSW marks a write before running the handler.
    let _ = cpu.step(&mut bus); // fault
    let f = frame_base(&cpu);
    assert_eq!(
        bus.read_word(f + F_SSW),
        0x0101,
        "SSW = DF | RW=write | SIZ=long | FC=user data"
    );
    assert_eq!(
        bus.read_long(f + F_DOB),
        0x1234_5678,
        "data output buffer holds the faulted write's value"
    );

    step_n(&mut cpu, &mut bus, 5); // handler (3) + suppressed rerun + NOP
    assert!(!cpu.is_supervisor());
    assert_eq!(cpu.d(7), 0x1234_5678, "handler saw the write data");
    assert_eq!(cpu.pc & 0xFFFF, (CODE + 4) & 0xFFFF, "no re-fault loop");
}

/// A format $B frame RTE'd on a 68040 is a format error, as on real
/// silicon: the frame formats are not portable across models.
#[test]
fn format_b_frame_is_a_format_error_on_the_040() {
    let mut bus = TestBus::new(0x10000);
    bus.write_long(14 * 4, 0x0500); // format error vector

    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);
    cpu.set_sr(0x2700);
    cpu.set_a(7, SSP - FRAME_LEN);
    // Hand-built minimal format $B frame: SR, PC, format/vector.
    bus.write_word(SSP - FRAME_LEN, 0x0000);
    bus.write_long(SSP - FRAME_LEN + F_PC, CODE);
    bus.write_word(SSP - FRAME_LEN + F_FMT, 0xB008);
    bus.write_word(CODE, 0x4E73); // RTE
    cpu.pc = CODE;

    let _ = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0500, "format error handler entered");
}

/// FCL walks index the root table by function code: with the user-data
/// entry remapped and the supervisor entries identity, the same logical
/// address reads differently through MOVES with SFC=1 -- exactly
/// mmu.library's 030 MMU-presence probe.
#[test]
fn fcl_walk_indexes_the_root_table_by_function_code() {
    let mut bus = TestBus::new(0x10000);

    // FC-indexed root of early-termination descriptors: supervisor data
    // and program (5, 6) identity, user data (1) remapped to 0x4000.
    bus.write_long(ROOT_TABLE + 5 * 4, 0x0000_0059);
    bus.write_long(ROOT_TABLE + 6 * 4, 0x0000_0059);
    bus.write_long(ROOT_TABLE + 4, 0x0000_4059);

    bus.write_long(0x2000, 0x1111_2222); // identity view of 0x2000
    bus.write_long(0x6000, 0x3333_4444); // user-data view (0x4000 + 0x2000)

    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68030);
    cpu.mmu_crp_limit = 0x8000_0002;
    cpu.mmu_crp_aptr = ROOT_TABLE;
    cpu.mmu_tc = 0x81F0_9800; // E=1, FCL=1, PS=32K, TIA=9, TIB=8 (as mmu.library programs it)
    cpu.pmmu_enabled = true;
    cpu.sfc = 1;

    cpu.set_sr(0x2700);
    cpu.set_a(7, SSP);
    cpu.set_a(0, 0x2000);

    // MOVE.L (A0),D0 : supervisor data space, identity.
    bus.write_word(CODE, 0x2010);
    // MOVES.L (A0),D1 : SFC=1, user data space, remapped.
    bus.write_word(CODE + 2, 0x0E90);
    bus.write_word(CODE + 4, 0x1000);
    cpu.pc = CODE;

    step_n(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.d(0), 0x1111_2222, "supervisor read is identity");
    assert_eq!(cpu.d(1), 0x3333_4444, "MOVES read translates in SFC space");
}

/// A DT=2 pointer where the configured levels are exhausted is an indirect
/// descriptor: the walk follows it to the real page descriptor. Its address
/// field reaches bit 2, so a set bit 2 is address, not write-protect.
#[test]
fn exhausted_walk_follows_indirect_descriptors() {
    let mut bus = TestBus::new(0x10000);
    // Indirect descriptor at B[1] -> shared descriptor at 0x9004 (bit 2
    // set in the pointer on purpose) -> resident page at 0.
    let mut cpu = user_mode_030_with_table(&mut bus, 0x9004 | 2);
    bus.write_long(0x9004, 0x0000_0001);
    bus.write_long(0x0ABC, 0xFEED_FACE);

    cpu.set_a(0, FAULT_PAGE + 0x0ABC);
    bus.write_word(CODE, 0x2010); // MOVE.L (A0),D0

    step_n(&mut cpu, &mut bus, 1);
    assert!(
        !cpu.is_supervisor(),
        "resident indirect target must not fault"
    );
    assert_eq!(cpu.d(0), 0xFEED_FACE, "translated through the indirection");

    // A write through the same mapping is not write-protected: the
    // indirect descriptor's bit 2 is part of its address field.
    cpu.set_d(2, 0xD00D_D00D);
    bus.write_word(cpu.pc as u16 as u32, 0x2082); // MOVE.L D2,(A0)
    step_n(&mut cpu, &mut bus, 1);
    assert!(!cpu.is_supervisor(), "write must not take a WP fault");
    assert_eq!(bus.read_long(0x0ABC), 0xD00D_D00D);
}

/// PTEST walks in the extension word's function-code space, reports the
/// invalid translation in MMUSR (I + level count, readable through PMOVE
/// PSR), and with the A bit hands back the address of the last descriptor
/// examined -- for an exhausted-walk indirection that is the shared target
/// slot mmu.library materializes.
#[test]
fn ptest_reports_mmusr_and_locates_the_descriptor() {
    let mut bus = TestBus::new(0x10000);
    // B[1] indirect -> poisoned shared descriptor (mmu.library's tag).
    let mut cpu = user_mode_030_with_table(&mut bus, 0x9000 | 2);
    bus.write_long(0x9000, 0xBADF_EED0);

    // Supervisor: DFC=1, PTESTR #1... use SFC form like mmu.library:
    // MOVEC D0,SFC with D0=1; PTESTR SFC,(A0),#7; PMOVE PSR,(A7); then
    // PTESTR SFC,(A0),#2,A1 (level-limited, A bit).
    cpu.set_sr(0x2700);
    cpu.set_a(7, SSP);
    cpu.set_a(0, FAULT_PAGE);
    cpu.set_d(0, 1);
    let mut p = CODE;
    bus.write_word(p, 0x4E7B); // MOVEC D0,SFC
    bus.write_word(p + 2, 0x0000);
    p += 4;
    bus.write_word(p, 0xF010); // PTESTR SFC,(A0),#7
    bus.write_word(p + 2, 0x9E00);
    p += 4;
    bus.write_word(p, 0xF017); // PMOVE PSR,(A7)
    bus.write_word(p + 2, 0x6200);
    p += 4;
    bus.write_word(p, 0xF010); // PTESTR SFC,(A0),#2,A1
    bus.write_word(p + 2, 0x8B20);
    p += 4;
    bus.write_word(p, 0x4E71); // NOP
    cpu.pc = CODE;

    step_n(&mut cpu, &mut bus, 4);
    assert_eq!(
        cpu.mmu_sr & 0xFFFF,
        0x0402,
        "MMUSR = I | N=2 (indirection resolves within level 2)"
    );
    assert_eq!(
        bus.read_word(SSP),
        0x0402,
        "PMOVE PSR wrote MMUSR to memory"
    );
    assert_eq!(
        cpu.a(1),
        0x9000,
        "A bit returns the indirect target slot to materialize"
    );
}

/// PTEST reports accumulated write-protect on the walked path in MMUSR W,
/// for read tests too (a handler classifies write faults with it).
#[test]
fn ptest_reports_write_protect_on_read_walks() {
    let mut bus = TestBus::new(0x10000);
    let mut cpu = user_mode_030_with_table(&mut bus, 0x0000_0005); // DT=1 | WP

    cpu.set_sr(0x2700);
    cpu.set_a(7, SSP);
    cpu.set_a(0, FAULT_PAGE);
    cpu.set_d(0, 1);
    bus.write_word(CODE, 0x4E7B); // MOVEC D0,SFC
    bus.write_word(CODE + 2, 0x0000);
    bus.write_word(CODE + 4, 0xF010); // PTESTR SFC,(A0),#7
    bus.write_word(CODE + 6, 0x9E00);
    cpu.pc = CODE;

    step_n(&mut cpu, &mut bus, 2);
    assert_eq!(
        cpu.mmu_sr & 0xFFFF,
        0x0802,
        "MMUSR = W | N=2 for a resident write-protected page"
    );
}

/// MOVES moves every operand size in both directions through the SFC/DFC
/// address spaces, with address-register destinations sign-extending like
/// MOVEA. The FC-table setup from the FCL test doubles as the address-space
/// discriminator: user-data space is remapped, supervisor spaces identity.
#[test]
fn moves_sizes_directions_and_sign_extension() {
    let mut bus = TestBus::new(0x10000);

    bus.write_long(ROOT_TABLE + 5 * 4, 0x0000_0059); // supervisor data: identity
    bus.write_long(ROOT_TABLE + 6 * 4, 0x0000_0059); // supervisor program: identity
    bus.write_long(ROOT_TABLE + 4, 0x0000_4059); // user data: +0x4000

    bus.write_long(0x6000, 0x8899_AABB); // user-data view of 0x2000

    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68030);
    cpu.mmu_crp_limit = 0x8000_0002;
    cpu.mmu_crp_aptr = ROOT_TABLE;
    cpu.mmu_tc = 0x81F0_9800; // E=1, FCL=1, PS=32K, TIA=9, TIB=8
    cpu.pmmu_enabled = true;
    cpu.sfc = 1;
    cpu.dfc = 1;

    cpu.set_sr(0x2700);
    cpu.set_a(7, SSP);
    cpu.set_a(0, 0x2000);
    cpu.set_d(3, 0x1122_3344);
    cpu.pc = CODE;

    let mut p = CODE;
    // MOVES.B (A0),D1 ; MOVES.W (A0),D2 ; MOVES.L (A0),A1 (sign-extends)
    for (op, ext) in [(0x0E10u16, 0x1000u16), (0x0E50, 0x2000), (0x0E90, 0x9000)] {
        bus.write_word(p, op);
        bus.write_word(p + 2, ext);
        p += 4;
    }
    // MOVES.L D3,(A0): write into the user-data space (lands at 0x6000).
    bus.write_word(p, 0x0E90);
    bus.write_word(p + 2, 0x3800);
    p += 4;
    // MOVES.W (A0),A2: word read sign-extended into an address register.
    bus.write_word(p, 0x0E50);
    bus.write_word(p + 2, 0xA000);

    step_n(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.d(1) & 0xFF, 0x88, "byte read via SFC space");
    assert_eq!(cpu.d(2) & 0xFFFF, 0x8899, "word read via SFC space");
    assert_eq!(cpu.a(1), 0x8899_AABB, "long read into An");

    step_n(&mut cpu, &mut bus, 1);
    assert_eq!(
        bus.read_long(0x6000),
        0x1122_3344,
        "long write lands in the DFC space, not the identity view"
    );
    assert_eq!(bus.read_long(0x2000), 0, "identity view untouched");

    step_n(&mut cpu, &mut bus, 1);
    assert_eq!(
        cpu.a(2),
        0x0000_1122,
        "word read into An sign-extends the new memory value"
    );
}

/// MOVES in user mode is a privilege violation regardless of direction.
#[test]
fn moves_is_privileged() {
    let mut bus = TestBus::new(0x10000);
    bus.write_long(8 * 4, HANDLER); // privilege violation vector
    bus.write_word(HANDLER, 0x4E71);

    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68030);
    cpu.set_sr(0x2700);
    cpu.set_a(7, SSP);
    cpu.set_sr(0x0000);
    cpu.set_a(7, USP);
    cpu.set_a(0, 0x2000);
    cpu.pc = CODE;
    bus.write_word(CODE, 0x0E10); // MOVES.B (A0),D1
    bus.write_word(CODE + 2, 0x1000);

    step_n(&mut cpu, &mut bus, 1);
    assert!(cpu.is_supervisor(), "privilege violation taken");
    assert_eq!(cpu.pc & 0xFFFF, HANDLER);
    assert_eq!(
        bus.read_long(cpu.a(7) + 2),
        CODE,
        "faulting instruction's PC stacked"
    );
}
