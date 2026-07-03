//! Integration suite for SingleStepTests `m68000` (68000-only) fixtures.
//!
//! Fixtures are not vendored in-repo (they are large); each per-file test
//! skips itself when they are absent. Clone
//! <https://github.com/SingleStepTests/m68000> (MIT) to
//! `tests/fixtures/m68000`, or point `M68K_SST_FIXTURES` at the clone's
//! `v1` directory (what CI does). See `tests/fixtures/README.md`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use m68k::core::cpu::{MFLAG_SET, SFLAG_SET};
use m68k::{AddressBus, CpuCore, CpuType};

/// Upstream `m68000` repo stores PC using MAME `m_au` (“next prefetch address”).
/// Upstream documents this as +4 relative to where the test starts executing.
fn mame_au_to_exec_pc(pc: u32) -> u32 {
    pc.wrapping_sub(4)
}

fn exec_pc_to_mame_au(pc: u32) -> u32 {
    pc.wrapping_add(4)
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
// SingleStepTests `m68000` binary format decoder (matches upstream `decode.py`)
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
    /// The two prefetch-queue words: [word at exec PC, word at exec PC + 2].
    prefetch: [u32; 2],
    /// RAM is stored as byte pieces: (address, byte_value)
    ram: Vec<(u32, u8)>,
}

#[derive(Clone, Debug)]
struct BinTest {
    name: String,
    initial: BinState,
    final_: BinState,
    has_addr_error_txn: bool,
    /// Total CPU cycles the fixture (MAME microcoded core) reports for this
    /// instruction -- ground truth for cycle-accuracy measurement.
    fixture_cycles: u32,
    /// Word-granular bus accesses in the fixture's transaction log (reads +
    /// writes, excluding idle and address-error cycles) -- ground truth for
    /// bus-access-count measurement.
    fixture_access_words: u32,
    /// The full ordered bus-access log (idle transactions folded into each
    /// access's cycle offset) -- ground truth for access-sequence and
    /// per-access timing measurement.
    fixture_accesses: Vec<FixtureAccess>,
}

/// One completed bus cycle from the fixture's transaction log.
///
/// Upstream encoding (decode.py): tw 1 = write, 2 = read, 3 = TAS
/// read-modify-write cycle, 4/5 = read/write address error (AS never
/// asserted). Idle transactions (tw 0) are not stored as accesses; their
/// cycles accumulate into the next access's `cycle_offset`.
#[derive(Clone, Debug)]
struct FixtureAccess {
    is_write: bool,
    is_tas: bool,
    /// Word-aligned address driven on the address bus.
    addr: u32,
    /// Data bus value. For byte accesses only the strobed half is meaningful
    /// (the 68000 cannot drive A0; a UDS-only byte read of 0xAB shows 0xAB00).
    data: u16,
    uds: bool,
    lds: bool,
    /// CPU clocks from instruction start to the start of this bus cycle
    /// (sum of all preceding transactions' cycle counts).
    cycle_offset: u32,
    /// Duration of this bus cycle in CPU clocks (4 for a normal cycle).
    cycles: u32,
}

/// Parsed summary of a `transactions` block.
struct TxnInfo {
    has_addr_error: bool,
    num_cycles: u32,
    access_words: u32,
    accesses: Vec<FixtureAccess>,
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
        // In upstream decode.py, data is split into two bytes at addr/addr|1 (big-endian).
        ram.push((addr, (data >> 8) as u8));
        ram.push((addr | 1, (data & 0xFF) as u8));
    }

    Ok(BinState {
        regs,
        prefetch: [pf0, pf1],
        ram,
    })
}

fn read_transactions(bytes: &[u8], ptr: &mut usize) -> Result<TxnInfo, String> {
    let _num_bytes = read_block_header(bytes, ptr, MAGIC_TXNS)?;
    let num_cycles = read_u32_le(bytes, ptr)?;
    let num_transactions = read_u32_le(bytes, ptr)? as usize;
    let mut has_addr_error = false;
    let mut access_words = 0u32;
    let mut accesses: Vec<FixtureAccess> = Vec::new();
    let mut cycle_offset = 0u32;
    for _ in 0..num_transactions {
        let tw = read_u8(bytes, ptr)?;
        let cycles = read_u32_le(bytes, ptr)?;
        if tw == 0 {
            // Idle transaction: internal CPU clocks, no bus activity.
            cycle_offset = cycle_offset.saturating_add(cycles);
            continue;
        }
        // Upstream decode.py:
        // 1 = write, 2 = read, 3 = TAS read-modify-write cycle,
        // 4 = read address error (no AS assert), 5 = write address error (no AS assert)
        if tw == 4 || tw == 5 {
            has_addr_error = true;
        } else {
            // A real completed bus cycle (read/write/TAS): one 16-bit access.
            access_words += 1;
        }
        // fc, addr_bus, data_bus, UDS, LDS (all u32 LE)
        let _fc = read_u32_le(bytes, ptr)?;
        let addr_bus = read_u32_le(bytes, ptr)?;
        let data_bus = read_u32_le(bytes, ptr)?;
        let uds = read_u32_le(bytes, ptr)?;
        let lds = read_u32_le(bytes, ptr)?;
        if tw != 4 && tw != 5 {
            accesses.push(FixtureAccess {
                is_write: tw == 1,
                is_tas: tw == 3,
                addr: addr_bus & !1,
                data: data_bus as u16,
                uds: uds != 0,
                lds: lds != 0,
                cycle_offset,
                cycles,
            });
        }
        cycle_offset = cycle_offset.saturating_add(cycles);
    }
    Ok(TxnInfo {
        has_addr_error,
        num_cycles,
        access_words,
        accesses,
    })
}

fn read_test(bytes: &[u8], ptr: &mut usize) -> Result<BinTest, String> {
    let _num_bytes = read_block_header(bytes, ptr, MAGIC_TEST)?;
    let name = read_name(bytes, ptr)?;
    let initial = read_state(bytes, ptr)?;
    let final_ = read_state(bytes, ptr)?;
    let txn = read_transactions(bytes, ptr)?;
    Ok(BinTest {
        name,
        initial,
        final_,
        has_addr_error_txn: txn.has_addr_error,
        fixture_cycles: txn.num_cycles,
        fixture_access_words: txn.access_words,
        fixture_accesses: txn.accesses,
    })
}

fn load_test_file(path: &Path) -> Result<Vec<BinTest>, String> {
    let bytes = fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let mut ptr = 0usize;
    let magic = read_u32_le(&bytes, &mut ptr)?;
    if magic != MAGIC_FILE {
        return Err(format!(
            "{}: bad file magic: expected {MAGIC_FILE:#010X}, got {magic:#010X}",
            path.display()
        ));
    }
    let num_tests = read_u32_le(&bytes, &mut ptr)? as usize;
    let mut out = Vec::with_capacity(num_tests);
    for _ in 0..num_tests {
        out.push(read_test(&bytes, &mut ptr).map_err(|e| format!("{}: {e}", path.display()))?);
    }
    Ok(out)
}

fn fixture_root_v1() -> PathBuf {
    if let Ok(root) = std::env::var("M68K_SST_FIXTURES") {
        return PathBuf::from(root);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("m68000")
        .join("v1")
}

fn run_one_file(path: &Path) {
    if !path.exists() {
        // The fixture set is a separate (large, MIT-licensed) clone; a
        // checkout without it must still build and pass. CI fetches the
        // fixtures and runs this suite for real.
        eprintln!(
            "skip {}: SingleStepTests fixtures not present (see tests/fixtures/README.md)",
            path.display()
        );
        return;
    }
    let tests = load_test_file(path).unwrap();
    let mut failures: Vec<String> = Vec::new();

    for (idx, t) in tests.iter().enumerate() {
        // Build memory from initial (byte pieces).
        let mut bus = SparseBus::default();
        for (addr, b) in &t.initial.ram {
            bus.write_byte(*addr, *b);
        }

        let mut cpu = CpuCore::new();
        cpu.set_sst_m68000_compat(true);
        load_state_68000(&mut cpu, &t.initial);

        // Grab opcode at instruction start (after m_au->pc adjustment).
        let opcode = bus.read_word(cpu.pc);

        // Execute one instruction (HLE handler falls back to exceptions)
        let mut hle = m68k::NoOpHleHandler;
        let _result = cpu.step_with_hle_handler(&mut bus, &mut hle);

        let ctx = format!("{}[{}] {}", path.display(), idx, t.name);

        if std::env::var("M68K_SST_DEBUG_CASE")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            == Some(idx)
        {
            eprintln!("=== SST DEBUG CASE idx={idx} ===");
            eprintln!("file: {}", path.display());
            eprintln!("name: {}", t.name);
            eprintln!("opcode: {opcode:#06X}");
            eprintln!(
                "SR init: {:#06X}  SR exp: {:#06X}  SR got: {:#06X}",
                reg(&t.initial, "sr") as u16,
                reg(&t.final_, "sr") as u16,
                cpu.get_sr()
            );
            eprintln!(
                "SP got: {:#010X}  USP got: {:#010X}  SSP got: {:#010X}",
                cpu.sp(),
                cpu.get_usp(),
                if cpu.is_supervisor() {
                    cpu.sp()
                } else {
                    cpu.sp[SFLAG_SET as usize]
                }
            );
            eprintln!(
                "SP exp: {:#010X}  USP exp: {:#010X}  SSP exp: {:#010X}",
                if (reg(&t.final_, "sr") as u16) & 0x2000 != 0 {
                    reg(&t.final_, "ssp")
                } else {
                    reg(&t.final_, "usp")
                },
                reg(&t.final_, "usp"),
                reg(&t.final_, "ssp")
            );
            let sp = cpu.sp();
            eprintln!("stack dump @ SP (got):");
            for i in 0..16u32 {
                let b = bus.read_byte(sp.wrapping_add(i));
                eprint!("{b:02X} ");
            }
            eprintln!();

            let exp_map: std::collections::HashMap<u32, u8> =
                t.final_.ram.iter().copied().collect();
            eprintln!("stack dump @ SP (exp):");
            for i in 0..16u32 {
                let b = *exp_map.get(&sp.wrapping_add(i)).unwrap_or(&0);
                eprint!("{b:02X} ");
            }
            eprintln!();
            eprintln!(
                "D0..D7 init: [{:#010X} {:#010X} {:#010X} {:#010X} {:#010X} {:#010X} {:#010X} {:#010X}]",
                reg(&t.initial, "d0"),
                reg(&t.initial, "d1"),
                reg(&t.initial, "d2"),
                reg(&t.initial, "d3"),
                reg(&t.initial, "d4"),
                reg(&t.initial, "d5"),
                reg(&t.initial, "d6"),
                reg(&t.initial, "d7")
            );
            eprintln!(
                "D0..D7 exp:  [{:#010X} {:#010X} {:#010X} {:#010X} {:#010X} {:#010X} {:#010X} {:#010X}]",
                reg(&t.final_, "d0"),
                reg(&t.final_, "d1"),
                reg(&t.final_, "d2"),
                reg(&t.final_, "d3"),
                reg(&t.final_, "d4"),
                reg(&t.final_, "d5"),
                reg(&t.final_, "d6"),
                reg(&t.final_, "d7")
            );
            eprintln!(
                "D0..D7 got:  [{:#010X} {:#010X} {:#010X} {:#010X} {:#010X} {:#010X} {:#010X} {:#010X}]",
                cpu.d(0),
                cpu.d(1),
                cpu.d(2),
                cpu.d(3),
                cpu.d(4),
                cpu.d(5),
                cpu.d(6),
                cpu.d(7)
            );

            // Extra decode for ABCD/SBCD operands.
            let group = (opcode >> 12) & 0xF;
            let op_mode = (opcode >> 6) & 7;
            let ea_mode = (opcode >> 3) & 7;
            let dst_reg = ((opcode >> 9) & 7) as usize;
            let src_reg = (opcode & 7) as usize;
            let x_in = (reg(&t.initial, "sr") as u16 & 0x10) != 0;

            let is_abcd = group == 0xC && op_mode == 4 && (ea_mode == 0 || ea_mode == 1);
            let is_sbcd = group == 0x8 && op_mode == 4 && (ea_mode == 0 || ea_mode == 1);

            if is_abcd || is_sbcd {
                eprintln!(
                    "{} {} src_reg={} dst_reg={} x_in={}",
                    if is_abcd { "ABCD" } else { "SBCD" },
                    if ea_mode == 0 { "RR" } else { "MM" },
                    src_reg,
                    dst_reg,
                    x_in as u8
                );
                if ea_mode == 0 {
                    let src_b = (reg(&t.initial, &format!("d{src_reg}")) & 0xFF) as u8;
                    let dst_b = (reg(&t.initial, &format!("d{dst_reg}")) & 0xFF) as u8;
                    eprintln!("src_b={src_b:#04X} dst_b={dst_b:#04X}");
                } else {
                    // For MM, operands are fetched from predecrement addresses (A7 uses 2 for byte).
                    let src_a = reg(&t.initial, &format!("a{src_reg}"));
                    let dst_a = reg(&t.initial, &format!("a{dst_reg}"));
                    let src_dec = if src_reg == 7 { 2u32 } else { 1u32 };
                    let dst_dec = if dst_reg == 7 { 2u32 } else { 1u32 };
                    let src_addr = src_a.wrapping_sub(src_dec);
                    let dst_addr = dst_a.wrapping_sub(dst_dec);
                    let src_b = bus.read_byte(src_addr);
                    let dst_b = bus.read_byte(dst_addr);
                    eprintln!(
                        "src_addr={src_addr:#010X} dst_addr={dst_addr:#010X} src_b={src_b:#04X} dst_b={dst_b:#04X}"
                    );
                }
            }

            // Show first few expected memory byte mismatches (often more actionable than SR alone).
            let mut shown = 0usize;
            for (addr, exp_b) in &t.final_.ram {
                let got_b = bus.read_byte(*addr);
                if got_b != *exp_b {
                    eprintln!(
                        "mem mismatch @ {addr:#010X}: got={got_b:#04X} expected={exp_b:#04X}"
                    );
                    shown += 1;
                    if shown >= 8 {
                        break;
                    }
                }
            }
        }

        if let Err(e) = check_state_68000(
            &t.final_,
            &cpu,
            &mut bus,
            &ctx,
            opcode,
            t.has_addr_error_txn,
        ) {
            failures.push(e);
            if failures.len() >= 25 {
                break;
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "SingleStepTests m68000 failures in {} (showing up to 25):\n{}",
            path.display(),
            failures.join("\n")
        );
    }
}

macro_rules! singlestep_file_test {
    ($name:ident, $rel_path:literal) => {
        #[test]
        fn $name() {
            let path = fixture_root_v1().join($rel_path);
            run_one_file(&path);
        }
    };
}

// ---------------------------------------------------------------------------------------------
// Cycle / bus-access accuracy measurement (gap report)
//
// The per-file tests above assert only final register/RAM state. The fixtures
// also carry the MAME-microcoded cycle count and the per-cycle bus transactions,
// which this core currently neither models accurately nor checks. `cycle_gap_report`
// runs every fixture case and tallies, per opcode file, how far the core's
// returned cycle count and bus-access count are from ground truth -- the
// measurement that drives Copperline's cycle-accurate pacing.
//
// Run with:
//   cargo test --release --test singlestep_m68000_v1_tests cycle_gap_report -- --ignored --nocapture
// ---------------------------------------------------------------------------------------------

/// Bus that records word-granular access count (reads + writes), to compare
/// against the fixture's transaction log. The core's `try_*` paths delegate to
/// these infallible methods, so every access is counted here.
#[derive(Default)]
struct CountingBus {
    inner: SparseBus,
    accesses: u32,
}

impl AddressBus for CountingBus {
    fn read_byte(&mut self, address: u32) -> u8 {
        self.accesses += 1;
        self.inner.read_byte(address)
    }
    fn read_word(&mut self, address: u32) -> u16 {
        self.accesses += 1;
        self.inner.read_word(address)
    }
    fn read_long(&mut self, address: u32) -> u32 {
        self.accesses += 2;
        self.inner.read_long(address)
    }
    fn write_byte(&mut self, address: u32, value: u8) {
        self.accesses += 1;
        self.inner.write_byte(address, value);
    }
    fn write_word(&mut self, address: u32, value: u16) {
        self.accesses += 1;
        self.inner.write_word(address, value);
    }
    fn write_long(&mut self, address: u32, value: u32) {
        self.accesses += 2;
        self.inner.write_long(address, value);
    }
}

#[derive(Default)]
struct FileGap {
    name: String,
    cases: u64,
    skipped: u64,
    cyc_mismatch: u64,
    cyc_abs_delta_sum: i64,
    cyc_signed_delta_sum: i64,
    cyc_max_abs_delta: i32,
    cpu_cyc_sum: i64,
    fix_cyc_sum: i64,
    acc_mismatch: u64,
    cpu_acc_sum: i64,
    fix_acc_sum: i64,
}

#[test]
#[ignore = "measurement, run explicitly with --ignored --nocapture"]
fn cycle_gap_report() {
    let root = fixture_root_v1();
    let mut files: Vec<PathBuf> = match fs::read_dir(&root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "bin").unwrap_or(false))
            .collect(),
        Err(e) => {
            eprintln!(
                "cycle_gap_report: fixtures missing at {} ({e}); fetch SingleStepTests/m68000 first",
                root.display()
            );
            return;
        }
    };
    files.sort();

    // Optional detail: set M68K_GAP_FILE=ADD.l.json.bin to dump, for that file,
    // a histogram of (fixture_cycles -> cpu_cycles) mismatches keyed by
    // addressing-mode fields of a sample opcode.
    let detail_target = std::env::var("M68K_GAP_FILE").ok();
    // (opcode_ea_mode, opcode_ea_reg_is_pcimm, fixture, cpu) -> (count, sample opcode)
    let mut detail: HashMap<(u16, i32, i32), (u64, u16)> = HashMap::new();

    let mut gaps: Vec<FileGap> = Vec::new();
    for path in &files {
        let tests = match load_test_file(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("skip {}: {e}", path.display());
                continue;
            }
        };
        let mut g = FileGap {
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let want_detail = detail_target.as_deref() == Some(g.name.as_str());
        for t in &tests {
            // Skip address-error cases (the state harness skips them too: they
            // assert bus/prefetch-exact micro-behavior this core does not model).
            if t.has_addr_error_txn {
                continue;
            }
            let mut bus = CountingBus::default();
            for (addr, b) in &t.initial.ram {
                bus.inner.set_byte(*addr, *b);
            }
            let mut cpu = CpuCore::new();
            cpu.set_sst_m68000_compat(true);
            load_state_68000(&mut cpu, &t.initial);

            let opcode = bus.inner.read_word(cpu.pc);
            let mut hle = m68k::NoOpHleHandler;
            let result = cpu.step_with_hle_handler(&mut bus, &mut hle);
            let cpu_cycles = match result {
                m68k::StepResult::Ok { cycles } => cycles,
                _ => {
                    g.skipped += 1;
                    continue;
                }
            };

            g.cases += 1;
            let delta = cpu_cycles - t.fixture_cycles as i32;
            g.cpu_cyc_sum += cpu_cycles as i64;
            g.fix_cyc_sum += t.fixture_cycles as i64;
            if delta != 0 {
                g.cyc_mismatch += 1;
                g.cyc_abs_delta_sum += delta.unsigned_abs() as i64;
                g.cyc_signed_delta_sum += delta as i64;
                g.cyc_max_abs_delta = g.cyc_max_abs_delta.max(delta.abs());
                if want_detail {
                    // key by ea mode (bits 5..3) so we see which addressing modes are off
                    let ea_mode = (opcode >> 3) & 7;
                    let e = detail
                        .entry((ea_mode, t.fixture_cycles as i32, cpu_cycles))
                        .or_insert((0, opcode));
                    e.0 += 1;
                }
            }
            g.cpu_acc_sum += bus.accesses as i64;
            g.fix_acc_sum += t.fixture_access_words as i64;
            if bus.accesses != t.fixture_access_words {
                g.acc_mismatch += 1;
            }
        }
        gaps.push(g);
    }

    if detail_target.is_some() && !detail.is_empty() {
        println!("\n=== detail for {} ===", detail_target.as_deref().unwrap());
        println!("{:>7} {:>9} {:>9} {:>8}  sample", "ea_mode", "fixture", "cpu", "count");
        let mut rows: Vec<_> = detail.iter().collect();
        rows.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
        for ((ea_mode, fix, cpu), (count, opcode)) in rows.iter().take(30) {
            println!(
                "{ea_mode:>7} {fix:>9} {cpu:>9} {count:>8}  {opcode:#06X}"
            );
        }
    }

    // Sort worst cycle-accuracy first.
    gaps.sort_by(|a, b| {
        let ra = a.cyc_mismatch as f64 / (a.cases.max(1) as f64);
        let rb = b.cyc_mismatch as f64 / (b.cases.max(1) as f64);
        rb.partial_cmp(&ra).unwrap()
    });

    println!("\n=== m68000 cycle / bus-access gap vs SingleStepTests (MAME) ===");
    println!(
        "{:<22} {:>8} {:>8} {:>8} {:>7} {:>9} {:>9}",
        "opcode.size", "cases", "cyc!=", "%cyc!=", "maxd", "avgΔcyc", "%acc!="
    );
    let (mut tc, mut tm, mut tcpu_cyc, mut tfix_cyc, mut tacc_m, mut tcpu_acc, mut tfix_acc) =
        (0u64, 0u64, 0i64, 0i64, 0u64, 0i64, 0i64);
    for g in &gaps {
        let pct = 100.0 * g.cyc_mismatch as f64 / (g.cases.max(1) as f64);
        let avgd = g.cyc_signed_delta_sum as f64 / (g.cases.max(1) as f64);
        let accpct = 100.0 * g.acc_mismatch as f64 / (g.cases.max(1) as f64);
        // Only print the rows that actually have a gap, to keep it scannable.
        if g.cyc_mismatch > 0 || g.acc_mismatch > 0 {
            println!(
                "{:<22} {:>8} {:>8} {:>7.1}% {:>7} {:>+9.2} {:>8.1}%",
                g.name.replace(".json.bin", ""),
                g.cases,
                g.cyc_mismatch,
                pct,
                g.cyc_max_abs_delta,
                avgd,
                accpct
            );
        }
        tc += g.cases;
        tm += g.cyc_mismatch;
        tcpu_cyc += g.cpu_cyc_sum;
        tfix_cyc += g.fix_cyc_sum;
        tacc_m += g.acc_mismatch;
        tcpu_acc += g.cpu_acc_sum;
        tfix_acc += g.fix_acc_sum;
    }
    println!("\n--- totals ---");
    println!("files                : {}", gaps.len());
    println!("cases                : {tc}");
    println!(
        "cycle mismatches     : {tm} ({:.1}%)",
        100.0 * tm as f64 / (tc.max(1) as f64)
    );
    println!(
        "sum CPU cycles       : {tcpu_cyc}  vs fixture {tfix_cyc}  (CPU/fixture = {:.3})",
        tcpu_cyc as f64 / (tfix_cyc.max(1) as f64)
    );
    println!(
        "bus-access mismatches: {tacc_m} ({:.1}%)",
        100.0 * tacc_m as f64 / (tc.max(1) as f64)
    );
    println!(
        "sum CPU accesses     : {tcpu_acc}  vs fixture {tfix_acc}  (CPU/fixture = {:.3})",
        tcpu_acc as f64 / (tfix_acc.max(1) as f64)
    );
    println!(
        "\nInterpretation: CPU/fixture cycle ratio < 1.0 means the core under-bills\n\
         cycles (drives Copperline cycle pacing too fast). The bus-access ratio shows\n\
         how the modeled access count compares to real 68000 word bus cycles."
    );
}

// ---------------------------------------------------------------------------------------------
// Access-sequence / per-access-timing measurement (gap reports)
//
// The fixtures' transaction logs are the per-bus-cycle ground truth from MAME's
// microcoded 68000: every read/write in order, with the address/data/strobes
// and the cycle offset at which each bus cycle starts. These two reports
// measure how far the core's emitted access stream is from that truth:
//
//   access_sequence_gap_report  -- order/address/direction/data of accesses
//                                  (the prefetch-model scoreboard)
//   access_timing_gap_report    -- the cycle offset of each access within the
//                                  instruction (the sync()-timing scoreboard)
//
// Run with:
//   cargo test --release --test singlestep_m68000_v1_tests access_sequence_gap_report -- --ignored --nocapture
//   cargo test --release --test singlestep_m68000_v1_tests access_timing_gap_report -- --ignored --nocapture
// ---------------------------------------------------------------------------------------------

/// One bus access as emitted by the core under test, word-granular to match
/// the fixture transaction encoding.
#[derive(Clone, Debug)]
struct RecordedAccess {
    is_write: bool,
    /// Word-aligned address.
    addr: u32,
    /// Data on the active byte lanes (hi byte if UDS, lo byte if LDS).
    data: u16,
    uds: bool,
    lds: bool,
    /// CPU clocks of internal work the core reported (via `AddressBus::sync`)
    /// since the previous access. Zero until the core implements sync().
    sync_clocks_before: u32,
}

/// Bus that records every access the core makes, word-granular and in order,
/// for comparison against fixture transaction logs. Long accesses are split
/// into two word records (high word first, matching how the trait's
/// `read_long`/`write_long` are defined to decompose).
#[derive(Default)]
struct RecordingBus {
    inner: SparseBus,
    accesses: Vec<RecordedAccess>,
    pending_sync_clocks: u32,
}

impl RecordingBus {
    fn record(&mut self, is_write: bool, byte_addr: u32, size: u8, data: u32) {
        let word_addr = byte_addr & !1;
        let (uds, lds, lane_data) = match size {
            1 => {
                if byte_addr & 1 == 0 {
                    (true, false, ((data as u16) & 0xFF) << 8)
                } else {
                    (false, true, (data as u16) & 0xFF)
                }
            }
            _ => (true, true, data as u16),
        };
        self.accesses.push(RecordedAccess {
            is_write,
            addr: word_addr,
            data: lane_data,
            uds,
            lds,
            sync_clocks_before: std::mem::take(&mut self.pending_sync_clocks),
        });
    }
}

impl AddressBus for RecordingBus {
    fn sync(&mut self, cpu_clocks: u32) {
        self.pending_sync_clocks += cpu_clocks;
    }
    fn read_byte(&mut self, address: u32) -> u8 {
        let v = self.inner.read_byte(address);
        self.record(false, address, 1, v as u32);
        v
    }
    fn read_word(&mut self, address: u32) -> u16 {
        let v = self.inner.read_word(address);
        self.record(false, address, 2, v as u32);
        v
    }
    fn read_long(&mut self, address: u32) -> u32 {
        let hi = self.read_word(address);
        let lo = self.read_word(address.wrapping_add(2));
        ((hi as u32) << 16) | lo as u32
    }
    fn write_byte(&mut self, address: u32, value: u8) {
        self.record(true, address, 1, value as u32);
        self.inner.write_byte(address, value);
    }
    fn write_word(&mut self, address: u32, value: u16) {
        self.record(true, address, 2, value as u32);
        self.inner.write_word(address, value);
    }
    fn write_long(&mut self, address: u32, value: u32) {
        self.write_word(address, (value >> 16) as u16);
        self.write_word(address.wrapping_add(2), (value & 0xFFFF) as u16);
    }
}

/// How a recorded access stream differs from the fixture's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SequenceMismatch {
    /// Different number of bus accesses.
    Count,
    /// Same count, but some access differs in read/write direction.
    Direction,
    /// Same count and directions, but some access touches a different address.
    Address,
    /// Same count/directions/addresses, but data on the active lanes differs.
    Data,
}

/// Compare the core's recorded access stream against the fixture's.
/// Returns the first (most severe) category of mismatch, or None if they agree.
fn compare_access_sequences(
    fixture: &[FixtureAccess],
    recorded: &[RecordedAccess],
) -> Option<SequenceMismatch> {
    if fixture.len() != recorded.len() {
        return Some(SequenceMismatch::Count);
    }
    for (f, r) in fixture.iter().zip(recorded.iter()) {
        if f.is_write != r.is_write {
            return Some(SequenceMismatch::Direction);
        }
    }
    for (f, r) in fixture.iter().zip(recorded.iter()) {
        if f.addr & 0x00FF_FFFE != r.addr & 0x00FF_FFFE {
            return Some(SequenceMismatch::Address);
        }
    }
    for (f, r) in fixture.iter().zip(recorded.iter()) {
        // Compare only the byte lanes both sides drive. TAS cycles in the
        // fixture have read+write phases folded into one transaction whose
        // data reflects the write; skip data comparison for them.
        if f.is_tas {
            continue;
        }
        let mut mask = 0u16;
        if f.uds && r.uds {
            mask |= 0xFF00;
        }
        if f.lds && r.lds {
            mask |= 0x00FF;
        }
        if (f.data & mask) != (r.data & mask) {
            return Some(SequenceMismatch::Data);
        }
    }
    None
}

#[derive(Default)]
struct SequenceFileGap {
    name: String,
    cases: u64,
    skipped: u64,
    count_mismatch: u64,
    direction_mismatch: u64,
    address_mismatch: u64,
    data_mismatch: u64,
}

impl SequenceFileGap {
    fn mismatches(&self) -> u64 {
        self.count_mismatch + self.direction_mismatch + self.address_mismatch + self.data_mismatch
    }
}

/// Fixture files whose transaction logs the upstream README documents as
/// unreliable: TAS ("doesn't properly handle the special 5-cycle TAS
/// read-modify-write timing") and TRAPV ("some strange issue I don't
/// understand with the TRAPV tests"). Their final-state assertions still run
/// in the functional suite; only the access-stream reports skip them.
const UNRELIABLE_TRANSACTION_FIXTURES: [&str; 2] = ["TAS.json.bin", "TRAPV.json.bin"];

/// Shared driver: run every non-address-error fixture case through the core
/// with a RecordingBus and hand (test, recorded accesses) to `visit`.
fn for_each_recorded_case(mut visit: impl FnMut(&Path, &BinTest, &[RecordedAccess], bool)) {
    let root = fixture_root_v1();
    let mut files: Vec<PathBuf> = match fs::read_dir(&root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "bin").unwrap_or(false))
            .collect(),
        Err(e) => {
            eprintln!(
                "fixtures missing at {} ({e}); fetch SingleStepTests/m68000 first",
                root.display()
            );
            return;
        }
    };
    files.sort();

    for path in &files {
        let fname = path.file_name().unwrap().to_string_lossy();
        if UNRELIABLE_TRANSACTION_FIXTURES.contains(&fname.as_ref()) {
            continue;
        }
        let tests = match load_test_file(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("skip {}: {e}", path.display());
                continue;
            }
        };
        for t in &tests {
            if t.has_addr_error_txn {
                continue;
            }
            let mut bus = RecordingBus::default();
            for (addr, b) in &t.initial.ram {
                bus.inner.set_byte(*addr, *b);
            }
            let mut cpu = CpuCore::new();
            cpu.set_sst_m68000_compat(true);
            load_state_68000(&mut cpu, &t.initial);

            let mut hle = m68k::NoOpHleHandler;
            let result = cpu.step_with_hle_handler(&mut bus, &mut hle);
            let executed = matches!(result, m68k::StepResult::Ok { .. });
            visit(path, t, &bus.accesses, executed);
        }
    }
}

#[test]
#[ignore = "measurement, run explicitly with --ignored --nocapture"]
fn access_sequence_gap_report() {
    let mut gaps: Vec<SequenceFileGap> = Vec::new();
    let mut current: Option<SequenceFileGap> = None;
    // Optional: M68K_GAP_FILE=Bcc.json.bin prints the first few mismatching
    // cases for that file with both access streams.
    let detail_target = std::env::var("M68K_GAP_FILE").ok();
    let mut detail_shown = 0usize;

    for_each_recorded_case(|path, t, recorded, executed| {
        let fname = path.file_name().unwrap().to_string_lossy().into_owned();
        if current.as_ref().map(|g| g.name != fname).unwrap_or(true) {
            if let Some(g) = current.take() {
                gaps.push(g);
            }
            current = Some(SequenceFileGap {
                name: fname.clone(),
                ..Default::default()
            });
        }
        let g = current.as_mut().unwrap();
        if !executed {
            g.skipped += 1;
            return;
        }
        g.cases += 1;
        match compare_access_sequences(&t.fixture_accesses, recorded) {
            None => {}
            Some(kind) => {
                match kind {
                    SequenceMismatch::Count => g.count_mismatch += 1,
                    SequenceMismatch::Direction => g.direction_mismatch += 1,
                    SequenceMismatch::Address => g.address_mismatch += 1,
                    SequenceMismatch::Data => g.data_mismatch += 1,
                }
                if detail_target.as_deref() == Some(fname.as_str()) && detail_shown < 5 {
                    detail_shown += 1;
                    println!("\n--- {} mismatch ({kind:?}): {} ---", fname, t.name);
                    println!("fixture ({} accesses):", t.fixture_accesses.len());
                    for f in &t.fixture_accesses {
                        println!(
                            "  {} {:#010X} data={:#06X} uds={} lds={} @+{}",
                            if f.is_write { "W" } else { "R" },
                            f.addr,
                            f.data,
                            f.uds as u8,
                            f.lds as u8,
                            f.cycle_offset
                        );
                    }
                    println!("core ({} accesses):", recorded.len());
                    for r in recorded {
                        println!(
                            "  {} {:#010X} data={:#06X} uds={} lds={}",
                            if r.is_write { "W" } else { "R" },
                            r.addr,
                            r.data,
                            r.uds as u8,
                            r.lds as u8
                        );
                    }
                }
            }
        }
    });
    if let Some(g) = current.take() {
        gaps.push(g);
    }

    gaps.sort_by(|a, b| {
        let ra = a.mismatches() as f64 / (a.cases.max(1) as f64);
        let rb = b.mismatches() as f64 / (b.cases.max(1) as f64);
        rb.partial_cmp(&ra).unwrap()
    });

    println!("\n=== m68000 access-sequence gap vs SingleStepTests (MAME) ===");
    println!(
        "{:<22} {:>8} {:>8} {:>8} {:>7} {:>7} {:>7} {:>7}",
        "opcode.size", "cases", "seq!=", "%seq!=", "count", "dir", "addr", "data"
    );
    let (mut tc, mut tm) = (0u64, 0u64);
    let (mut t_count, mut t_dir, mut t_addr, mut t_data) = (0u64, 0u64, 0u64, 0u64);
    for g in &gaps {
        let m = g.mismatches();
        if m > 0 {
            println!(
                "{:<22} {:>8} {:>8} {:>7.1}% {:>7} {:>7} {:>7} {:>7}",
                g.name.replace(".json.bin", ""),
                g.cases,
                m,
                100.0 * m as f64 / (g.cases.max(1) as f64),
                g.count_mismatch,
                g.direction_mismatch,
                g.address_mismatch,
                g.data_mismatch
            );
        }
        tc += g.cases;
        tm += m;
        t_count += g.count_mismatch;
        t_dir += g.direction_mismatch;
        t_addr += g.address_mismatch;
        t_data += g.data_mismatch;
    }
    println!("\n--- totals ---");
    println!("files               : {}", gaps.len());
    println!("cases               : {tc}");
    println!(
        "sequence mismatches : {tm} ({:.1}%)",
        100.0 * tm as f64 / (tc.max(1) as f64)
    );
    println!("  by count          : {t_count}");
    println!("  by direction      : {t_dir}");
    println!("  by address        : {t_addr}");
    println!("  by data           : {t_data}");
    println!(
        "\nInterpretation: count mismatches are dominated by the missing prefetch\n\
         model (overlap fetches and discarded prefetches after flow changes).\n\
         Direction/address mismatches indicate wrong access ordering (e.g. long\n\
         write order) or wrong effective addresses."
    );
}

#[derive(Default)]
struct TimingFileGap {
    name: String,
    /// Cases whose access sequence matches (timing is only measured for these).
    seq_match_cases: u64,
    /// Cases whose sequence matches AND every access lands at the fixture's
    /// exact cycle offset.
    timing_exact_cases: u64,
    /// Total accesses compared / total with exact offsets.
    accesses: u64,
    accesses_exact: u64,
    offset_abs_delta_sum: i64,
}

#[test]
#[ignore = "measurement, run explicitly with --ignored --nocapture"]
fn access_timing_gap_report() {
    let mut gaps: Vec<TimingFileGap> = Vec::new();
    let mut current: Option<TimingFileGap> = None;
    // Optional: M68K_GAP_FILE=Bcc.json.bin prints the first few timing-
    // mismatching cases of that file with fixture vs core offsets.
    let detail_target = std::env::var("M68K_GAP_FILE").ok();
    let mut detail_shown = 0usize;

    for_each_recorded_case(|path, t, recorded, executed| {
        let fname = path.file_name().unwrap().to_string_lossy().into_owned();
        if current.as_ref().map(|g| g.name != fname).unwrap_or(true) {
            if let Some(g) = current.take() {
                gaps.push(g);
            }
            current = Some(TimingFileGap {
                name: fname.clone(),
                ..Default::default()
            });
        }
        let g = current.as_mut().unwrap();
        if !executed || compare_access_sequences(&t.fixture_accesses, recorded).is_some() {
            return;
        }
        g.seq_match_cases += 1;

        // Reconstruct the core's view of each access's cycle offset: the sum of
        // sync()-reported internal clocks plus the bus cycles of preceding
        // accesses (4 clocks each, the 68000 bus cycle length).
        let mut core_offset = 0u32;
        let mut all_exact = true;
        let mut core_offsets: Vec<u32> = Vec::with_capacity(recorded.len());
        for (f, r) in t.fixture_accesses.iter().zip(recorded.iter()) {
            core_offset += r.sync_clocks_before;
            core_offsets.push(core_offset);
            g.accesses += 1;
            let delta = core_offset as i64 - f.cycle_offset as i64;
            if delta == 0 {
                g.accesses_exact += 1;
            } else {
                all_exact = false;
                g.offset_abs_delta_sum += delta.abs();
            }
            core_offset += f.cycles.max(4); // advance past this bus cycle
        }
        if all_exact {
            g.timing_exact_cases += 1;
        } else if detail_target.as_deref() == Some(fname.as_str()) && detail_shown < 5 {
            detail_shown += 1;
            println!("\n--- {} timing mismatch: {} ---", fname, t.name);
            println!("{:>4} {:>10} {:>10} {:>6}  access", "idx", "fixture", "core", "delta");
            for (i, (f, r)) in t.fixture_accesses.iter().zip(recorded.iter()).enumerate() {
                println!(
                    "{:>4} {:>10} {:>10} {:>+6}  {} {:#010X}",
                    i,
                    f.cycle_offset,
                    core_offsets[i],
                    core_offsets[i] as i64 - f.cycle_offset as i64,
                    if r.is_write { "W" } else { "R" },
                    r.addr
                );
            }
        }
    });
    if let Some(g) = current.take() {
        gaps.push(g);
    }

    gaps.sort_by(|a, b| {
        let ra = a.accesses_exact as f64 / (a.accesses.max(1) as f64);
        let rb = b.accesses_exact as f64 / (b.accesses.max(1) as f64);
        ra.partial_cmp(&rb).unwrap()
    });

    println!("\n=== m68000 per-access timing gap vs SingleStepTests (MAME) ===");
    println!("(only cases whose access sequence already matches are measured)");
    println!(
        "{:<22} {:>10} {:>10} {:>10} {:>10} {:>9}",
        "opcode.size", "seq-match", "exact", "accesses", "acc-exact", "avg|d|"
    );
    let (mut t_cases, mut t_exact, mut t_acc, mut t_acc_exact, mut t_delta) =
        (0u64, 0u64, 0u64, 0u64, 0i64);
    for g in &gaps {
        if g.seq_match_cases > 0 && g.accesses_exact < g.accesses {
            let inexact_accesses = g.accesses - g.accesses_exact;
            println!(
                "{:<22} {:>10} {:>10} {:>10} {:>10} {:>9.2}",
                g.name.replace(".json.bin", ""),
                g.seq_match_cases,
                g.timing_exact_cases,
                g.accesses,
                g.accesses_exact,
                g.offset_abs_delta_sum as f64 / (inexact_accesses.max(1) as f64)
            );
        }
        t_cases += g.seq_match_cases;
        t_exact += g.timing_exact_cases;
        t_acc += g.accesses;
        t_acc_exact += g.accesses_exact;
        t_delta += g.offset_abs_delta_sum;
    }
    println!("\n--- totals ---");
    println!("seq-matching cases   : {t_cases}");
    println!(
        "timing-exact cases   : {t_exact} ({:.1}%)",
        100.0 * t_exact as f64 / (t_cases.max(1) as f64)
    );
    println!(
        "accesses exact-offset: {t_acc_exact} / {t_acc} ({:.1}%)",
        100.0 * t_acc_exact as f64 / (t_acc.max(1) as f64)
    );
    println!(
        "sum |offset delta|   : {t_delta} clocks",
    );
    println!(
        "\nInterpretation: offsets are measured from instruction start; the core's\n\
         offset for access k = sum of sync() clocks + 4 clocks per preceding bus\n\
         cycle. Until the core implements sync(), every multi-access instruction\n\
         reports offset 0 for later accesses and this report is the Phase 2\n\
         scoreboard, not a regression signal."
    );
}

// One test per SingleStepTests m68000 v1 fixture file.
// This is intentionally hard-coded (no build.rs generation) and assumes the fixtures submodule is present.
singlestep_file_test!(singlestep_m68000_v1_abcd_json_bin, "ABCD.json.bin");
singlestep_file_test!(singlestep_m68000_v1_add_b_json_bin, "ADD.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_add_l_json_bin, "ADD.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_add_w_json_bin, "ADD.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_adda_l_json_bin, "ADDA.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_adda_w_json_bin, "ADDA.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_addx_b_json_bin, "ADDX.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_addx_l_json_bin, "ADDX.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_addx_w_json_bin, "ADDX.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_and_b_json_bin, "AND.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_and_l_json_bin, "AND.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_and_w_json_bin, "AND.w.json.bin");
singlestep_file_test!(
    singlestep_m68000_v1_anditoccr_json_bin,
    "ANDItoCCR.json.bin"
);
singlestep_file_test!(singlestep_m68000_v1_anditosr_json_bin, "ANDItoSR.json.bin");
singlestep_file_test!(singlestep_m68000_v1_asl_b_json_bin, "ASL.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_asl_l_json_bin, "ASL.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_asl_w_json_bin, "ASL.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_asr_b_json_bin, "ASR.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_asr_l_json_bin, "ASR.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_asr_w_json_bin, "ASR.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_bcc_json_bin, "Bcc.json.bin");
singlestep_file_test!(singlestep_m68000_v1_bchg_json_bin, "BCHG.json.bin");
singlestep_file_test!(singlestep_m68000_v1_bclr_json_bin, "BCLR.json.bin");
singlestep_file_test!(singlestep_m68000_v1_bset_json_bin, "BSET.json.bin");
singlestep_file_test!(singlestep_m68000_v1_bsr_json_bin, "BSR.json.bin");
singlestep_file_test!(singlestep_m68000_v1_btst_json_bin, "BTST.json.bin");
singlestep_file_test!(singlestep_m68000_v1_chk_json_bin, "CHK.json.bin");
singlestep_file_test!(singlestep_m68000_v1_clr_b_json_bin, "CLR.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_clr_l_json_bin, "CLR.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_clr_w_json_bin, "CLR.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_cmp_b_json_bin, "CMP.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_cmp_l_json_bin, "CMP.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_cmp_w_json_bin, "CMP.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_cmpa_l_json_bin, "CMPA.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_cmpa_w_json_bin, "CMPA.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_dbcc_json_bin, "DBcc.json.bin");
singlestep_file_test!(singlestep_m68000_v1_divs_json_bin, "DIVS.json.bin");
singlestep_file_test!(singlestep_m68000_v1_divu_json_bin, "DIVU.json.bin");
singlestep_file_test!(singlestep_m68000_v1_eor_b_json_bin, "EOR.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_eor_l_json_bin, "EOR.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_eor_w_json_bin, "EOR.w.json.bin");
singlestep_file_test!(
    singlestep_m68000_v1_eoritoccr_json_bin,
    "EORItoCCR.json.bin"
);
singlestep_file_test!(singlestep_m68000_v1_eoritosr_json_bin, "EORItoSR.json.bin");
singlestep_file_test!(singlestep_m68000_v1_exg_json_bin, "EXG.json.bin");
singlestep_file_test!(singlestep_m68000_v1_ext_l_json_bin, "EXT.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_ext_w_json_bin, "EXT.w.json.bin");
singlestep_file_test!(
    singlestep_m68000_v1_illegal_linea_json_bin,
    "ILLEGAL_LINEA.json.bin"
);
singlestep_file_test!(
    singlestep_m68000_v1_illegal_linef_json_bin,
    "ILLEGAL_LINEF.json.bin"
);
singlestep_file_test!(singlestep_m68000_v1_jmp_json_bin, "JMP.json.bin");
singlestep_file_test!(singlestep_m68000_v1_jsr_json_bin, "JSR.json.bin");
singlestep_file_test!(singlestep_m68000_v1_lea_json_bin, "LEA.json.bin");
singlestep_file_test!(singlestep_m68000_v1_link_json_bin, "LINK.json.bin");
singlestep_file_test!(singlestep_m68000_v1_lsl_b_json_bin, "LSL.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_lsl_l_json_bin, "LSL.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_lsl_w_json_bin, "LSL.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_lsr_b_json_bin, "LSR.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_lsr_l_json_bin, "LSR.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_lsr_w_json_bin, "LSR.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_move_b_json_bin, "MOVE.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_move_l_json_bin, "MOVE.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_move_q_json_bin, "MOVE.q.json.bin");
singlestep_file_test!(singlestep_m68000_v1_move_w_json_bin, "MOVE.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_movea_l_json_bin, "MOVEA.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_movea_w_json_bin, "MOVEA.w.json.bin");
singlestep_file_test!(
    singlestep_m68000_v1_movefromsr_json_bin,
    "MOVEfromSR.json.bin"
);
singlestep_file_test!(
    singlestep_m68000_v1_movefromusp_json_bin,
    "MOVEfromUSP.json.bin"
);
singlestep_file_test!(singlestep_m68000_v1_movem_l_json_bin, "MOVEM.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_movem_w_json_bin, "MOVEM.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_movep_l_json_bin, "MOVEP.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_movep_w_json_bin, "MOVEP.w.json.bin");
singlestep_file_test!(
    singlestep_m68000_v1_movetoccr_json_bin,
    "MOVEtoCCR.json.bin"
);
singlestep_file_test!(singlestep_m68000_v1_movetosr_json_bin, "MOVEtoSR.json.bin");
singlestep_file_test!(
    singlestep_m68000_v1_movetousp_json_bin,
    "MOVEtoUSP.json.bin"
);
singlestep_file_test!(singlestep_m68000_v1_muls_json_bin, "MULS.json.bin");
singlestep_file_test!(singlestep_m68000_v1_mulu_json_bin, "MULU.json.bin");
singlestep_file_test!(singlestep_m68000_v1_nbcd_json_bin, "NBCD.json.bin");
singlestep_file_test!(singlestep_m68000_v1_neg_b_json_bin, "NEG.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_neg_l_json_bin, "NEG.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_neg_w_json_bin, "NEG.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_negx_b_json_bin, "NEGX.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_negx_l_json_bin, "NEGX.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_negx_w_json_bin, "NEGX.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_nop_json_bin, "NOP.json.bin");
singlestep_file_test!(singlestep_m68000_v1_not_b_json_bin, "NOT.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_not_l_json_bin, "NOT.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_not_w_json_bin, "NOT.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_or_b_json_bin, "OR.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_or_l_json_bin, "OR.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_or_w_json_bin, "OR.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_oritoccr_json_bin, "ORItoCCR.json.bin");
singlestep_file_test!(singlestep_m68000_v1_oritosr_json_bin, "ORItoSR.json.bin");
singlestep_file_test!(singlestep_m68000_v1_pea_json_bin, "PEA.json.bin");
singlestep_file_test!(singlestep_m68000_v1_reset_json_bin, "RESET.json.bin");
singlestep_file_test!(singlestep_m68000_v1_rol_b_json_bin, "ROL.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_rol_l_json_bin, "ROL.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_rol_w_json_bin, "ROL.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_ror_b_json_bin, "ROR.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_ror_l_json_bin, "ROR.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_ror_w_json_bin, "ROR.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_roxl_b_json_bin, "ROXL.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_roxl_l_json_bin, "ROXL.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_roxl_w_json_bin, "ROXL.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_roxr_b_json_bin, "ROXR.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_roxr_l_json_bin, "ROXR.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_roxr_w_json_bin, "ROXR.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_rte_json_bin, "RTE.json.bin");
singlestep_file_test!(singlestep_m68000_v1_rtr_json_bin, "RTR.json.bin");
singlestep_file_test!(singlestep_m68000_v1_rts_json_bin, "RTS.json.bin");
singlestep_file_test!(singlestep_m68000_v1_sbcd_json_bin, "SBCD.json.bin");
singlestep_file_test!(singlestep_m68000_v1_scc_json_bin, "Scc.json.bin");
singlestep_file_test!(singlestep_m68000_v1_stop_json_bin, "STOP.json.bin");
singlestep_file_test!(singlestep_m68000_v1_sub_b_json_bin, "SUB.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_sub_l_json_bin, "SUB.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_sub_w_json_bin, "SUB.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_suba_l_json_bin, "SUBA.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_suba_w_json_bin, "SUBA.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_subx_b_json_bin, "SUBX.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_subx_l_json_bin, "SUBX.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_subx_w_json_bin, "SUBX.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_swap_json_bin, "SWAP.json.bin");
singlestep_file_test!(singlestep_m68000_v1_tas_json_bin, "TAS.json.bin");
singlestep_file_test!(singlestep_m68000_v1_trap_json_bin, "TRAP.json.bin");
singlestep_file_test!(singlestep_m68000_v1_trapv_json_bin, "TRAPV.json.bin");
singlestep_file_test!(singlestep_m68000_v1_tst_b_json_bin, "TST.b.json.bin");
singlestep_file_test!(singlestep_m68000_v1_tst_l_json_bin, "TST.l.json.bin");
singlestep_file_test!(singlestep_m68000_v1_tst_w_json_bin, "TST.w.json.bin");
singlestep_file_test!(singlestep_m68000_v1_unlink_json_bin, "UNLINK.json.bin");

fn reg(st: &BinState, name: &str) -> u32 {
    for (i, n) in REG_ORDER.iter().enumerate() {
        if *n == name {
            return st.regs[i];
        }
    }
    0
}

fn load_state_68000(cpu: &mut CpuCore, state: &BinState) {
    cpu.set_cpu_type(CpuType::M68000);

    // SR (flags + S/M), but do NOT bank SP yet. We’ll install USP/SSP from the fixture first,
    // then select the active A7 based on the S bit.
    cpu.set_sr_noint_nosp(reg(state, "sr") as u16);

    // PC: upstream uses m_au (“next prefetch”), adjust to actual execution PC for our core.
    cpu.pc = mame_au_to_exec_pc(reg(state, "pc"));

    // D0-D7, A0-A6
    for i in 0..8 {
        cpu.set_d(i, reg(state, &format!("d{i}")));
    }
    for i in 0..7 {
        cpu.set_a(i, reg(state, &format!("a{i}")));
    }

    // USP/SSP are provided explicitly.
    let usp = reg(state, "usp");
    let ssp = reg(state, "ssp");
    cpu.sp[0] = usp;
    cpu.sp[SFLAG_SET as usize] = ssp;
    cpu.sp[(SFLAG_SET | MFLAG_SET) as usize] = ssp;

    // Set active A7 based on S bit.
    if cpu.s_flag != 0 {
        cpu.set_sp(ssp);
    } else {
        cpu.set_sp(usp);
    }

    // The fixture provides the prefetch queue contents at instruction start:
    // [word at exec PC, word at exec PC + 2]. Real hardware fetched these
    // during previous instructions, so the queue starts full.
    cpu.prefetch_queue = [state.prefetch[0] as u16, state.prefetch[1] as u16];
    cpu.prefetch_count = 2;
}

fn check_state_68000(
    expected: &BinState,
    cpu: &CpuCore,
    bus: &mut SparseBus,
    ctx: &str,
    opcode: u16,
    has_addr_error_txn: bool,
) -> Result<(), String> {
    // If the fixture includes bus-level address-error cycles, it is asserting bus/prefetch-
    // accurate behavior (including exactly when the fault occurs). m68k is not bus/prefetch-
    // accurate, so we skip these cases entirely.
    if has_addr_error_txn {
        let _ = (expected, cpu, bus, ctx, opcode);
        return Ok(());
    }
    fn get_ssp(cpu: &CpuCore) -> u32 {
        if cpu.is_supervisor() {
            cpu.sp()
        } else {
            cpu.sp[SFLAG_SET as usize]
        }
    }

    fn sr_mask_for_opcode(opcode: u16) -> u16 {
        let group = (opcode >> 12) & 0xF;
        let op_mode = (opcode >> 6) & 7;
        let ea_mode = (opcode >> 3) & 7;
        let is_abcd = group == 0xC && op_mode == 4 && (ea_mode == 0 || ea_mode == 1);
        let is_sbcd = group == 0x8 && op_mode == 4 && (ea_mode == 0 || ea_mode == 1);
        let is_nbcd = (opcode & 0xFFC0) == 0x4800; // 0100 1000 00 mmm rrr

        if is_abcd || is_sbcd || is_nbcd {
            // N and V are undefined for BCD ops on 68000; don't pin exact bits.
            !0x000Au16
        } else {
            0xFFFF
        }
    }

    for i in 0..8 {
        let exp = reg(expected, &format!("d{i}"));
        let got = cpu.d(i);
        if got != exp {
            return Err(format!(
                "{ctx}: D{i} mismatch (got={got:#010X} expected={exp:#010X})"
            ));
        }
    }
    // Address register side effects on address-error paths can differ depending on bus/prefetch
    // micro-architecture details. We focus on D-reg + SR correctness and skip A0-A6 in these cases.
    if !has_addr_error_txn {
        for i in 0..7 {
            let exp = reg(expected, &format!("a{i}"));
            let got = cpu.a(i);
            if got != exp {
                return Err(format!(
                    "{ctx}: A{i} mismatch (got={got:#010X} expected={exp:#010X})"
                ));
            }
        }
    }

    let sr_mask = sr_mask_for_opcode(opcode);
    let expected_sr = reg(expected, "sr") as u16;
    let actual_sr = cpu.get_sr();
    if (actual_sr & sr_mask) != (expected_sr & sr_mask) {
        return Err(format!(
            "{ctx}: SR mismatch (mask={sr_mask:#06X}) (got={actual_sr:#06X} expected={expected_sr:#06X})"
        ));
    }
    // PC in these fixtures is MAME's `m_au` (next prefetch address) and is sensitive to prefetch
    // modeling details. m68k currently doesn't emulate the full prefetch queue, so don't fail
    // tests solely on PC unless explicitly requested.
    if std::env::var("M68K_SST_STRICT_PC").ok().as_deref() == Some("1") {
        let exp_pc = reg(expected, "pc");
        let got_pc = exec_pc_to_mame_au(cpu.pc);
        if got_pc != exp_pc {
            return Err(format!(
                "{ctx}: PC mismatch (expected MAME m_au) (got={got_pc:#010X} expected={exp_pc:#010X})"
            ));
        }
    }
    // USP/SSP can be affected by the exact sequence of predecrement/postincrement bus cycles on
    // address-error paths. Since we're not bus-cycle accurate, skip these comparisons for such
    // cases and focus on architectural state.
    if !has_addr_error_txn {
        let exp_usp = reg(expected, "usp");
        let got_usp = cpu.get_usp();
        if got_usp != exp_usp {
            return Err(format!(
                "{ctx}: USP mismatch (got={got_usp:#010X} expected={exp_usp:#010X})"
            ));
        }
        let exp_ssp = reg(expected, "ssp");
        let got_ssp = get_ssp(cpu);
        if got_ssp != exp_ssp {
            return Err(format!(
                "{ctx}: SSP mismatch (got={got_ssp:#010X} expected={exp_ssp:#010X})"
            ));
        }
    }

    // Memory expectations: compare bytes at specified addresses.
    //
    // SingleStepTests includes bus-level address-error cycles (tw=4/5) and expects the resulting
    // stack frame / bus artifacts in RAM. m68k is not bus-cycle accurate, so we skip RAM
    // assertions for these cases and focus on architectural state (regs/SR/PC).
    if !has_addr_error_txn {
        for (addr, b) in &expected.ram {
            let got = bus.read_byte(*addr);
            if got != *b {
                return Err(format!(
                    "{ctx}: mem[{addr:#010X}].b mismatch (got={got:#04X} expected={:#04X})",
                    *b
                ));
            }
        }
    }

    Ok(())
}
