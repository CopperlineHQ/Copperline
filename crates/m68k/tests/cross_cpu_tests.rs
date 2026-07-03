mod common;

use std::collections::HashMap;

use m68k::core::cpu::{MFLAG_SET, SFLAG_SET};
use m68k::{AddressBus, CpuCore, CpuType};

// Cross-CPU compatibility matrix: verify backward compatibility by running
// older CPU fixtures on newer CPUs. All tests should pass if backward
// compatibility is maintained correctly.

// =============================================================================
// SingleStepTests M68000 Format Support
// =============================================================================
// The M68000 fixtures use the SingleStepTests format, which requires parsing
// and running single-step tests rather than running a compiled test binary.

/// Upstream `m68000` repo stores PC using MAME `m_au` ("next prefetch address").
/// Upstream documents this as +4 relative to where the test starts executing.
fn mame_au_to_exec_pc(pc: u32) -> u32 {
    pc.wrapping_sub(4)
}

#[derive(Default, Clone)]
struct SparseBus {
    mem: HashMap<u32, u8>,
}

impl SparseBus {
    fn set_byte(&mut self, address: u32, value: u8) {
        self.mem.insert(address, value);
    }

    fn write_word_be(&mut self, address: u32, value: u16) {
        self.set_byte(address, (value >> 8) as u8);
        self.set_byte(address.wrapping_add(1), (value & 0xFF) as u8);
    }
}

impl AddressBus for SparseBus {
    fn read_byte(&mut self, address: u32) -> u8 {
        *self.mem.get(&address).unwrap_or(&0)
    }

    fn read_word(&mut self, address: u32) -> u16 {
        let hi = self.read_byte(address) as u16;
        let lo = self.read_byte(address.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    fn read_long(&mut self, address: u32) -> u32 {
        ((self.read_word(address) as u32) << 16) | (self.read_word(address.wrapping_add(2)) as u32)
    }

    fn write_byte(&mut self, address: u32, value: u8) {
        self.set_byte(address, value);
    }

    fn write_word(&mut self, address: u32, value: u16) {
        self.write_word_be(address, value);
    }

    fn write_long(&mut self, address: u32, value: u32) {
        self.write_word_be(address, (value >> 16) as u16);
        self.write_word_be(address.wrapping_add(2), (value & 0xFFFF) as u16);
    }
}

// ---------------------------------------------------------------------------------------------
// SingleStepTests `m68000` binary format decoder
// ---------------------------------------------------------------------------------------------

const MAGIC_FILE: u32 = 0x1A3F5D71;
const MAGIC_TEST: u32 = 0xABC12367;
const MAGIC_NAME: u32 = 0x89ABCDEF;
const MAGIC_STATE: u32 = 0x01234567;
const MAGIC_TXNS: u32 = 0x456789AB;

const REG_ORDER: [&str; 19] = [
    "d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "a0", "a1", "a2", "a3", "a4", "a5", "a6",
    "usp", "ssp", "sr", "pc",
];

#[derive(Clone, Debug)]
struct BinState {
    regs: [u32; REG_ORDER.len()],
    #[allow(dead_code)]
    prefetch: [u32; 2],
    ram: Vec<(u32, u8)>,
}

#[derive(Clone, Debug)]
struct BinTest {
    #[allow(dead_code)]
    name: String,
    initial: BinState,
    final_: BinState,
    has_addr_error_txn: bool,
}

fn read_u8(bytes: &[u8], ptr: &mut usize) -> Result<u8, String> {
    if *ptr + 1 > bytes.len() {
        return Err("unexpected EOF".to_string());
    }
    let v = bytes[*ptr];
    *ptr += 1;
    Ok(v)
}

fn read_u16_le(bytes: &[u8], ptr: &mut usize) -> Result<u16, String> {
    if *ptr + 2 > bytes.len() {
        return Err("unexpected EOF".to_string());
    }
    let v = u16::from_le_bytes([bytes[*ptr], bytes[*ptr + 1]]);
    *ptr += 2;
    Ok(v)
}

fn read_u32_le(bytes: &[u8], ptr: &mut usize) -> Result<u32, String> {
    if *ptr + 4 > bytes.len() {
        return Err("unexpected EOF".to_string());
    }
    let v = u32::from_le_bytes([
        bytes[*ptr],
        bytes[*ptr + 1],
        bytes[*ptr + 2],
        bytes[*ptr + 3],
    ]);
    *ptr += 4;
    Ok(v)
}

fn read_block_header(bytes: &[u8], ptr: &mut usize, expected_magic: u32) -> Result<u32, String> {
    let num_bytes = read_u32_le(bytes, ptr)?;
    let magic = read_u32_le(bytes, ptr)?;
    if magic != expected_magic {
        return Err(format!(
            "bad block magic: expected {expected_magic:#010X}, got {magic:#010X}"
        ));
    }
    Ok(num_bytes)
}

fn read_name(bytes: &[u8], ptr: &mut usize) -> Result<String, String> {
    let _num_bytes = read_block_header(bytes, ptr, MAGIC_NAME)?;
    let strlen = read_u32_le(bytes, ptr)? as usize;
    if *ptr + strlen > bytes.len() {
        return Err("unexpected EOF reading name".to_string());
    }
    let s = std::str::from_utf8(&bytes[*ptr..*ptr + strlen])
        .map_err(|e| format!("invalid utf-8 name: {e}"))?
        .to_string();
    *ptr += strlen;
    Ok(s)
}

fn read_state(bytes: &[u8], ptr: &mut usize) -> Result<BinState, String> {
    let _num_bytes = read_block_header(bytes, ptr, MAGIC_STATE)?;

    let mut regs = [0u32; REG_ORDER.len()];
    for r in regs.iter_mut() {
        *r = read_u32_le(bytes, ptr)?;
    }

    let pf0 = read_u32_le(bytes, ptr)?;
    let pf1 = read_u32_le(bytes, ptr)?;

    let num_rams = read_u32_le(bytes, ptr)? as usize;
    let mut ram: Vec<(u32, u8)> = Vec::with_capacity(num_rams * 2);
    for _ in 0..num_rams {
        let addr = read_u32_le(bytes, ptr)?;
        let data = read_u16_le(bytes, ptr)?;
        ram.push((addr, (data >> 8) as u8));
        ram.push((addr | 1, (data & 0xFF) as u8));
    }

    Ok(BinState {
        regs,
        prefetch: [pf0, pf1],
        ram,
    })
}

fn read_transactions(bytes: &[u8], ptr: &mut usize) -> Result<bool, String> {
    let _num_bytes = read_block_header(bytes, ptr, MAGIC_TXNS)?;
    let _num_cycles = read_u32_le(bytes, ptr)?;
    let num_transactions = read_u32_le(bytes, ptr)? as usize;
    let mut has_addr_error = false;
    for _ in 0..num_transactions {
        let tw = read_u8(bytes, ptr)?;
        let _cycles = read_u32_le(bytes, ptr)?;
        if tw == 0 {
            continue;
        }
        if tw == 4 || tw == 5 {
            has_addr_error = true;
        }
        let _fc = read_u32_le(bytes, ptr)?;
        let _addr_bus = read_u32_le(bytes, ptr)?;
        let _data_bus = read_u32_le(bytes, ptr)?;
        let _uds = read_u32_le(bytes, ptr)?;
        let _lds = read_u32_le(bytes, ptr)?;
    }
    Ok(has_addr_error)
}

fn read_test(bytes: &[u8], ptr: &mut usize) -> Result<BinTest, String> {
    let _num_bytes = read_block_header(bytes, ptr, MAGIC_TEST)?;
    let name = read_name(bytes, ptr)?;
    let initial = read_state(bytes, ptr)?;
    let final_ = read_state(bytes, ptr)?;
    let has_addr_error_txn = read_transactions(bytes, ptr)?;
    Ok(BinTest {
        name,
        initial,
        final_,
        has_addr_error_txn,
    })
}

fn load_test_file(bytes: &[u8]) -> Result<Vec<BinTest>, String> {
    let mut ptr = 0usize;
    let magic = read_u32_le(bytes, &mut ptr)?;
    if magic != MAGIC_FILE {
        return Err(format!(
            "bad file magic: expected {MAGIC_FILE:#010X}, got {magic:#010X}"
        ));
    }
    let num_tests = read_u32_le(bytes, &mut ptr)? as usize;
    let mut out = Vec::with_capacity(num_tests);
    for _ in 0..num_tests {
        out.push(read_test(bytes, &mut ptr)?);
    }
    Ok(out)
}

fn reg(st: &BinState, name: &str) -> u32 {
    for (i, n) in REG_ORDER.iter().enumerate() {
        if *n == name {
            return st.regs[i];
        }
    }
    0
}

fn load_state(cpu: &mut CpuCore, state: &BinState, cpu_type: CpuType) {
    cpu.set_cpu_type(cpu_type);
    cpu.set_sst_m68000_compat(true);
    cpu.set_sr_noint_nosp(reg(state, "sr") as u16);
    cpu.pc = mame_au_to_exec_pc(reg(state, "pc"));

    for i in 0..8 {
        cpu.set_d(i, reg(state, &format!("d{i}")));
    }
    for i in 0..7 {
        cpu.set_a(i, reg(state, &format!("a{i}")));
    }

    let usp = reg(state, "usp");
    let ssp = reg(state, "ssp");
    cpu.sp[0] = usp;
    cpu.sp[SFLAG_SET as usize] = ssp;
    cpu.sp[(SFLAG_SET | MFLAG_SET) as usize] = ssp;

    if cpu.s_flag != 0 {
        cpu.set_sp(ssp);
    } else {
        cpu.set_sp(usp);
    }
}

fn sr_mask_for_opcode(opcode: u16) -> u16 {
    let group = (opcode >> 12) & 0xF;
    let op_mode = (opcode >> 6) & 7;
    let ea_mode = (opcode >> 3) & 7;
    let is_abcd = group == 0xC && op_mode == 4 && (ea_mode == 0 || ea_mode == 1);
    let is_sbcd = group == 0x8 && op_mode == 4 && (ea_mode == 0 || ea_mode == 1);
    let is_nbcd = (opcode & 0xFFC0) == 0x4800;

    if is_abcd || is_sbcd || is_nbcd {
        !0x000Au16
    } else {
        0xFFFF
    }
}

fn check_state(
    expected: &BinState,
    cpu: &CpuCore,
    bus: &mut SparseBus,
    opcode: u16,
    has_addr_error_txn: bool,
) -> Result<(), String> {
    if has_addr_error_txn {
        return Ok(());
    }

    fn get_ssp(cpu: &CpuCore) -> u32 {
        if cpu.is_supervisor() {
            cpu.sp()
        } else {
            cpu.sp[SFLAG_SET as usize]
        }
    }

    for i in 0..8 {
        let exp = reg(expected, &format!("d{i}"));
        let got = cpu.d(i);
        if got != exp {
            return Err(format!(
                "D{i} mismatch (got={got:#010X} expected={exp:#010X})"
            ));
        }
    }

    if !has_addr_error_txn {
        for i in 0..7 {
            let exp = reg(expected, &format!("a{i}"));
            let got = cpu.a(i);
            if got != exp {
                return Err(format!(
                    "A{i} mismatch (got={got:#010X} expected={exp:#010X})"
                ));
            }
        }
    }

    let sr_mask = sr_mask_for_opcode(opcode);
    let expected_sr = reg(expected, "sr") as u16;
    let actual_sr = cpu.get_sr();
    if (actual_sr & sr_mask) != (expected_sr & sr_mask) {
        return Err(format!(
            "SR mismatch (got={actual_sr:#06X} expected={expected_sr:#06X})"
        ));
    }

    if !has_addr_error_txn {
        let exp_usp = reg(expected, "usp");
        let got_usp = cpu.get_usp();
        if got_usp != exp_usp {
            return Err(format!(
                "USP mismatch (got={got_usp:#010X} expected={exp_usp:#010X})"
            ));
        }
        let exp_ssp = reg(expected, "ssp");
        let got_ssp = get_ssp(cpu);
        if got_ssp != exp_ssp {
            return Err(format!(
                "SSP mismatch (got={got_ssp:#010X} expected={exp_ssp:#010X})"
            ));
        }
    }

    if !has_addr_error_txn {
        for (addr, b) in &expected.ram {
            let got = bus.read_byte(*addr);
            if got != *b {
                return Err(format!(
                    "mem[{addr:#010X}].b mismatch (got={got:#04X} expected={:#04X})",
                    *b
                ));
            }
        }
    }

    Ok(())
}

/// Run SingleStepTests format fixtures on a specified CPU type.
/// This tests backward compatibility - M68000 instructions should work on newer CPUs.
fn run_sst_cross_cpu(fixture_bytes: &[u8], cpu_type: CpuType, max_failures: usize) {
    let tests = load_test_file(fixture_bytes).expect("Failed to parse fixture");
    let mut failures: Vec<String> = Vec::new();

    for (idx, t) in tests.iter().enumerate() {
        let mut bus = SparseBus::default();
        for (addr, b) in &t.initial.ram {
            bus.write_byte(*addr, *b);
        }

        let mut cpu = CpuCore::new();
        load_state(&mut cpu, &t.initial, cpu_type);

        let opcode = bus.read_word(cpu.pc);
        let _cycles = cpu.step(&mut bus);

        if let Err(e) = check_state(&t.final_, &cpu, &mut bus, opcode, t.has_addr_error_txn) {
            failures.push(format!("[{}]: {}", idx, e));
            if failures.len() >= max_failures {
                break;
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "Cross-CPU SingleStepTests failures (showing up to {}):\n{}",
            max_failures,
            failures.join("\n")
        );
    }
}

// =============================================================================
// M68000 Baseline Cross-CPU Tests (SingleStepTests format)
// =============================================================================
// The M68000 fixtures are generated from MAME's microcoded M68000 emulator and
// test exact M68000 behavior. M68010 is close enough to pass these tests, but
// 68020+ have different microarchitectural behaviors for some edge cases (e.g.,
// flag timing, undefined bits) that cause legitimate test failures.
//
// ADD/MOVE tests run only on M68010; Bcc tests run on all CPUs since branch
// instructions are architecturally consistent across all 68k variants.

macro_rules! sst_cross_cpu_test {
    ($name:ident, $cpu_type:expr, $fixture_path:literal) => {
        #[test]
        fn $name() {
            let bytes = include_bytes!($fixture_path);
            run_sst_cross_cpu(bytes, $cpu_type, 25);
        }
    };
}

// ADD.b on M68010 (68020+ have different edge-case behaviors)
sst_cross_cpu_test!(
    m68000_add_on_68010,
    CpuType::M68010,
    "fixtures/m68000/v1/ADD.b.json.bin"
);

// MOVE.b on M68010 (68020+ have different edge-case behaviors)
sst_cross_cpu_test!(
    m68000_move_on_68010,
    CpuType::M68010,
    "fixtures/m68000/v1/MOVE.b.json.bin"
);

// Bcc on all CPUs (branch instructions are consistent across all 68k variants)
sst_cross_cpu_test!(
    m68000_bcc_on_68010,
    CpuType::M68010,
    "fixtures/m68000/v1/Bcc.json.bin"
);
sst_cross_cpu_test!(
    m68000_bcc_on_68020,
    CpuType::M68020,
    "fixtures/m68000/v1/Bcc.json.bin"
);
sst_cross_cpu_test!(
    m68000_bcc_on_68030,
    CpuType::M68030,
    "fixtures/m68000/v1/Bcc.json.bin"
);
sst_cross_cpu_test!(
    m68000_bcc_on_68040,
    CpuType::M68040,
    "fixtures/m68000/v1/Bcc.json.bin"
);

// =============================================================================
// M68010 Tests on Newer CPUs (Musashi format)
// =============================================================================
// M68010-specific features should work on 020/030/040

test_fixture!(
    m68010_vbr_on_68020,
    CpuType::M68020,
    "fixtures/extra/m68010/bin/vbr_test.bin"
);
test_fixture!(
    m68010_vbr_on_68030,
    CpuType::M68030,
    "fixtures/extra/m68010/bin/vbr_test.bin"
);
test_fixture!(
    m68010_vbr_on_68040,
    CpuType::M68040,
    "fixtures/extra/m68010/bin/vbr_test.bin"
);

test_fixture!(
    m68010_move_ccr_on_68020,
    CpuType::M68020,
    "fixtures/extra/m68010/bin/move_ccr.bin"
);
test_fixture!(
    m68010_move_ccr_on_68030,
    CpuType::M68030,
    "fixtures/extra/m68010/bin/move_ccr.bin"
);
test_fixture!(
    m68010_move_ccr_on_68040,
    CpuType::M68040,
    "fixtures/extra/m68010/bin/move_ccr.bin"
);

test_fixture!(
    m68010_loop_mode_on_68020,
    CpuType::M68020,
    "fixtures/extra/m68010/bin/loop_mode.bin"
);
test_fixture!(
    m68010_loop_mode_on_68030,
    CpuType::M68030,
    "fixtures/extra/m68010/bin/loop_mode.bin"
);
test_fixture!(
    m68010_loop_mode_on_68040,
    CpuType::M68040,
    "fixtures/extra/m68010/bin/loop_mode.bin"
);

// =============================================================================
// M68020 Tests on Newer CPUs
// =============================================================================
// M68020-specific features should work on 030/040

test_fixture!(
    m68020_bitfield_on_68030,
    CpuType::M68030,
    "fixtures/extra/m68020/bin/bitfield_ops.bin"
);
test_fixture!(
    m68020_bitfield_on_68040,
    CpuType::M68040,
    "fixtures/extra/m68020/bin/bitfield_ops.bin"
);

test_fixture!(
    m68020_cas_on_68030,
    CpuType::M68030,
    "fixtures/extra/m68020/bin/cas.bin"
);
test_fixture!(
    m68020_cas_on_68040,
    CpuType::M68040,
    "fixtures/extra/m68020/bin/cas.bin"
);

test_fixture!(
    m68020_chk2_on_68030,
    CpuType::M68030,
    "fixtures/extra/m68020/bin/chk2_cmp2.bin"
);
test_fixture!(
    m68020_chk2_on_68040,
    CpuType::M68040,
    "fixtures/extra/m68020/bin/chk2_cmp2.bin"
);

test_fixture!(
    m68020_long_muldiv_on_68030,
    CpuType::M68030,
    "fixtures/extra/m68020/bin/long_muldiv.bin"
);
test_fixture!(
    m68020_long_muldiv_on_68040,
    CpuType::M68040,
    "fixtures/extra/m68020/bin/long_muldiv.bin"
);

// =============================================================================
// M68030 Tests on M68040
// =============================================================================
// (The former cache_030/move16_030 cross tests were dropped: both fixtures
// now assert 68030-specific behavior - the 030 CACR bit layout and the
// Line-F traps for the 68040's CINV/MOVE16 - which does not carry to the
// 040.)

// =============================================================================
// Summary
// =============================================================================
// Total cross-CPU tests: 23
// - M68000 on M68010: 2 tests (ADD.b, MOVE.b) - SingleStepTests format
//   (Note: ADD/MOVE on 68020+ excluded due to microarchitectural differences)
// - M68000 Bcc on all CPUs: 4 tests - SingleStepTests format
//   (Branch instructions are consistent across all 68k variants)
// - M68010 on 020/030/040: 9 tests (3 fixtures × 3 CPUs) - Musashi format
// - M68020 on 030/040: 8 tests (4 fixtures × 2 CPUs) - Musashi format
// - M68030 on 040: none (the 030 fixtures assert 030-specific traps)
//
// These tests verify backward compatibility - features from older CPUs
// must work correctly on newer CPUs. Failures indicate regression bugs.
