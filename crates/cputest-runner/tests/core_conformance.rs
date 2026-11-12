//! Asset-free tests of the exact published core consumed by Copperline.
//! The generated instruction corpus cannot enable a PMMU or heat a JIT loop.

use m68k::core::trace_profile;
use m68k::mmu::{translate_address, MmuFaultKind};
use m68k::{AddressBus, CpuCore, CpuType, FastMem};

const MODELS: [CpuType; 4] = [
    CpuType::M68020,
    CpuType::M68030,
    CpuType::M68040,
    CpuType::M68060,
];
const MMUS: [CpuType; 3] = [CpuType::M68030, CpuType::M68040, CpuType::M68060];
const ROOT: u32 = 0x40000;
const PAGES: u32 = 0x42000;
const CODE: u32 = 0x1000;
const LOGICAL: u32 = 0x8000;
const PHYSICAL: u32 = 0x30000;

#[derive(Clone)]
struct Bus(Vec<u8>);
impl Bus {
    fn new(program: &[u16]) -> Self {
        let mut bus = Self(vec![0; 0x100000]);
        for (i, word) in program.iter().enumerate() {
            bus.write_word(CODE + i as u32 * 2, *word);
        }
        bus
    }
}
impl AddressBus for Bus {
    fn read_byte(&mut self, addr: u32) -> u8 {
        self.0[addr as usize]
    }
    fn read_word(&mut self, addr: u32) -> u16 {
        u16::from_be_bytes(self.0[addr as usize..addr as usize + 2].try_into().unwrap())
    }
    fn read_long(&mut self, addr: u32) -> u32 {
        u32::from_be_bytes(self.0[addr as usize..addr as usize + 4].try_into().unwrap())
    }
    fn write_byte(&mut self, addr: u32, val: u8) {
        self.0[addr as usize] = val;
    }
    fn write_word(&mut self, addr: u32, val: u16) {
        self.0[addr as usize..addr as usize + 2].copy_from_slice(&val.to_be_bytes());
    }
    fn write_long(&mut self, addr: u32, val: u32) {
        self.0[addr as usize..addr as usize + 4].copy_from_slice(&val.to_be_bytes());
    }
    fn fast_mem(&mut self) -> Option<FastMem> {
        Some(FastMem {
            ptr: self.0.as_mut_ptr(),
            base: 0,
            len: self.0.len() as u32,
        })
    }
}

fn cpu(model: CpuType) -> CpuCore {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(model);
    cpu.set_sr(0x2700);
    cpu.set_a(7, 0xf000);
    cpu.pc = CODE;
    cpu
}

fn enable_mmu(cpu: &mut CpuCore, bus: &mut Bus) {
    assert!(cpu.has_pmmu);
    cpu.mmu_crp_aptr = ROOT;
    cpu.mmu_srp_aptr = ROOT;
    if cpu.cpu_type == CpuType::M68030 {
        // 030 TC: enabled, 4 KiB pages, ignore top 8 bits, one 12-bit index;
        // CRP DT=2 selects short descriptors (MC68030UM, chapter 9).
        cpu.mmu_tc = 0x80c8_c000;
        // Lower limit of zero: every root index is in range (a limit word
        // of $00000002 is an upper limit of zero).
        cpu.mmu_crp_limit = 0x8000_0002;
        for page in 0..4096 {
            bus.write_long(ROOT + page * 4, (page * 4096) | 1);
        }
    } else {
        // 040/060 fixed root -> pointer -> page tables, 4 KiB pages.
        cpu.mmu_tc = 0x8000;
        bus.write_long(ROOT, (ROOT + 0x1000) | 2);
        bus.write_long(ROOT + 0x1000, PAGES | 2);
        for page in 0..64 {
            bus.write_long(PAGES + page * 4, (page * 4096) | 1);
        }
    }
    bus.write_long(descriptor(cpu), PHYSICAL | 1);
    cpu.pmmu_enabled = true;
}

fn descriptor(cpu: &CpuCore) -> u32 {
    if cpu.cpu_type == CpuType::M68030 {
        ROOT + 8 * 4
    } else {
        PAGES + 8 * 4
    }
}

fn flush(cpu: &mut CpuCore) {
    cpu.atc.flush_all();
    cpu.atc_030.flush_all();
}

#[test]
fn mmu_translation_permissions_and_atc_invalidation() {
    for model in MMUS {
        let mut cpu = cpu(model);
        let mut bus = Bus::new(&[]);
        enable_mmu(&mut cpu, &mut bus);
        for instruction in [false, true] {
            assert_eq!(
                translate_address(
                    &mut cpu,
                    &mut bus,
                    LOGICAL + 0x123,
                    false,
                    true,
                    instruction
                )
                .unwrap(),
                PHYSICAL + 0x123,
                "{model:?}"
            );
        }
        bus.write_long(descriptor(&cpu), 0x32001);
        assert_eq!(
            translate_address(&mut cpu, &mut bus, LOGICAL, false, true, false).unwrap(),
            PHYSICAL
        );
        // Execute the architectural PFLUSHA, rather than clearing the cache
        // through the API, before observing the changed mapping.
        if model == CpuType::M68030 {
            bus.write_word(CODE, 0xf000);
            bus.write_word(CODE + 2, 0x2400);
        } else {
            bus.write_word(CODE, 0xf518);
        }
        cpu.step(&mut bus);
        assert_eq!(cpu.last_exception_vector, None, "{model:?} PFLUSHA");
        assert_eq!(
            translate_address(&mut cpu, &mut bus, LOGICAL, false, true, false).unwrap(),
            0x32000
        );

        bus.write_long(descriptor(&cpu), PHYSICAL | 5); // Resident, write-protected.
        flush(&mut cpu);
        // Populate a read-only ATC entry, then ensure a write cannot bypass W.
        assert!(translate_address(&mut cpu, &mut bus, LOGICAL, false, true, false).is_ok());
        let fault = translate_address(&mut cpu, &mut bus, LOGICAL, true, true, false).unwrap_err();
        assert_eq!(fault.kind, MmuFaultKind::AccessLevelViolation, "{model:?}");
        assert_eq!(fault.address, LOGICAL);
        bus.write_long(descriptor(&cpu), 0); // Invalid page, data and code fault.
        flush(&mut cpu);
        for instruction in [false, true] {
            assert!(
                translate_address(&mut cpu, &mut bus, LOGICAL, false, true, instruction).is_err(),
                "{model:?} instruction={instruction}"
            );
        }
    }
}

#[test]
fn mmu_control_instructions_enable_translation_and_require_supervisor() {
    for model in MMUS {
        for supervisor in [true, false] {
            // PMOVE (A0),TC on 030; MOVEC D0,TC on 040/060.
            let program = if model == CpuType::M68030 {
                [0xf010, 0x4000]
            } else {
                [0x4e7b, 0x0003]
            };
            let mut bus = Bus::new(&program);
            let mut cpu = cpu(model);
            enable_mmu(&mut cpu, &mut bus);
            let tc = cpu.mmu_tc;
            cpu.pmmu_enabled = false;
            cpu.mmu_tc = 0;
            cpu.dar[0] = tc;
            cpu.set_a(0, 0x6000);
            bus.write_long(0x6000, tc);
            if !supervisor {
                cpu.set_sr(0);
            }
            cpu.step(&mut bus);
            assert_eq!(cpu.pmmu_enabled, supervisor, "{model:?}");
            if supervisor {
                assert_eq!(cpu.last_exception_vector, None);
                assert_eq!(
                    translate_address(&mut cpu, &mut bus, LOGICAL + 0x123, false, true, false)
                        .unwrap(),
                    PHYSICAL + 0x123
                );
            } else {
                assert_eq!(cpu.last_exception_vector, Some(8), "{model:?}");
                assert_eq!(cpu.mmu_tc, 0);
            }
        }
    }
}

#[test]
fn mmu_guest_load_and_write_fault_frames() {
    for model in MMUS {
        let mut cpu = cpu(model);
        let mut bus = Bus::new(&[0x2010]); // MOVE.L (A0),D0
        enable_mmu(&mut cpu, &mut bus);
        cpu.set_a(0, LOGICAL);
        bus.write_long(PHYSICAL, 0x1234_5678);
        cpu.step(&mut bus);
        assert_eq!(cpu.dar[0], 0x1234_5678, "{model:?}");
        assert_eq!(cpu.pc, CODE + 2);

        cpu.pc = CODE;
        bus.write_word(CODE, 0x2080); // MOVE.L D0,(A0)
        bus.write_long(2 * 4, 0x2000); // Bus/access-error vector.
        bus.write_long(descriptor(&cpu), PHYSICAL | 5);
        flush(&mut cpu);
        cpu.step(&mut bus);
        assert_eq!(cpu.last_exception_vector, Some(2), "{model:?}");
        assert_eq!(cpu.pc, 0x2000);
        assert_eq!(bus.read_long(PHYSICAL), 0x1234_5678);
        let sp = cpu.a(7);
        // MC68030UM 8.2 format B, MC68040UM 8.4 format 7,
        // MC68060UM 8.4 format 4: common SR/PC/format-vector prefix.
        let (format, size, fault_offset) = match model {
            CpuType::M68030 => (0xb008, 92, 0x10),
            CpuType::M68040 => (0x7008, 60, 0x14),
            CpuType::M68060 => (0x4008, 16, 0x08),
            _ => unreachable!(),
        };
        assert_eq!(sp, 0xf000 - size, "{model:?}");
        assert_eq!(bus.read_word(sp), 0x2700);
        assert_eq!(bus.read_long(sp + 2), CODE);
        assert_eq!(bus.read_word(sp + 6), format);
        assert_eq!(bus.read_long(sp + fault_offset), LOGICAL);
    }
}

fn compare(precise: &CpuCore, actual: &CpuCore, reference: &Bus, bus: &Bus) {
    assert_eq!(actual.dar, precise.dar);
    assert_eq!(actual.sp, precise.sp);
    assert_eq!(actual.pc, precise.pc);
    assert_eq!(actual.get_sr(), precise.get_sr());
    assert_eq!(bus.0, reference.0);
}

#[test]
fn native_jit_matches_interpreter_with_mmu_and_self_modifying_code() {
    // This crate always enables m68k/jit. On native targets its compiled
    // trace executor is Cranelift; a fallback to decoded dispatch never
    // increments these counters. With jit disabled the same counter also
    // counts portable traces, so keeping the Cargo feature enabled matters.
    const {
        assert!(
            !cfg!(target_family = "wasm"),
            "native JIT conformance needs a native host"
        )
    };
    for model in MODELS {
        for mmu in [false, true] {
            if mmu && model == CpuType::M68020 {
                continue;
            }
            // ADDQ.L #1,D0; ADD.L D0,D1; MOVE.L D1,(A1); BRA.S loop
            let mut bus = Bus::new(&[0x5280, 0xd280, 0x2281, 0x60f8]);
            let mut actual = cpu(model);
            actual.set_a(1, LOGICAL);
            if mmu {
                enable_mmu(&mut actual, &mut bus);
            }
            let mut reference = bus.clone();
            let mut precise = cpu(model);
            precise.set_a(1, LOGICAL);
            if mmu {
                enable_mmu(&mut precise, &mut reference);
            }
            for opcode in [0x5280, 0x5480] {
                // Rewrite the hot loop to ADDQ #2.
                bus.write_word(CODE, opcode);
                reference.write_word(CODE, opcode);
                trace_profile::reset();
                let result = actual.run_batch(&mut bus, 16384, &[]);
                assert_eq!(result.instructions, 16384, "{model:?} mmu={mmu}");
                let profile = trace_profile::snapshot();
                assert!(
                    profile.native_calls > 0 && profile.jit_retired > 0,
                    "no native trace ran: {model:?} mmu={mmu}"
                );
                for _ in 0..16384 {
                    precise.step(&mut reference);
                }
                compare(&precise, &actual, &reference, &bus);
            }
        }
    }
}
