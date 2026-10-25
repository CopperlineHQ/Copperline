//! Differential checks for the optional native dispatcher. The existing Rust driver is the
//! oracle; both sides use independently compiled instances of the same decoded guest program.
#![cfg(all(
    feature = "jit",
    not(target_family = "wasm"),
    not(feature = "trace-profile")
))]

use super::*;
use crate::core::memory::AddressBus;

const HEAD: u32 = 0x2000;
const CASE0: u32 = 0x2018;
const CASE1: u32 = 0x201e;
const CASE2: u32 = 0x2024;
/// A memory-storing arm only hot-edge discovery admits (command 3).
const CASE3: u32 = 0x202a;
const SOURCE: u32 = 0x3000;
const DESTINATION: u32 = 0x4000;
const COMMANDS: u32 = 0x5000;
const VALUE: u32 = 0x6000;
const RAM_LEN: usize = 0x8000;

/// Compare every CPU field except allocation identities and the opcode decode cache. This
/// retains raw flag words (including noncanonical values), rollback/exception state, timing,
/// and JIT observation/filter state; comparing get_ccr() alone would hide stale raw fields.
fn raw_state(cpu: &CpuCore) -> Vec<(&'static str, String)> {
    let mut fields = Vec::new();
    macro_rules! capture {
        ($($field:ident),* $(,)?) => { $(fields.push((stringify!($field), format!("{:?}", cpu.$field)));)* };
    }
    capture!(
        dar,
        dar_save,
        sr_save,
        ppc,
        pc,
        sp,
        vbr,
        sfc,
        dfc,
        cacr,
        caar,
        cacr_pending_ops,
        itt0,
        itt1,
        dtt0,
        dtt1,
        ir,
        fpr,
        fpiar,
        fpsr,
        fpcr,
        t1_flag,
        t0_flag,
        s_flag,
        m_flag,
        x_flag,
        n_flag,
        not_z_flag,
        v_flag,
        c_flag,
        int_mask,
        int_level,
        stopped,
        change_of_flow,
        loop_mode,
        loop_body_word,
        loop_dbcc_word,
        prefetch_queue,
        prefetch_count,
        consume_without_prefetch,
        pending_sync_clocks,
        precise_bus,
        cpu_type,
        address_mask,
        sr_mask,
        instr_mode,
        run_mode,
        exception_processing,
        instruction_exception_vector,
        bitfield_mem_wide_span,
        last_exception_vector,
        has_pmmu,
        pmmu_enabled,
        is_pre_68020,
        fpu_just_reset,
        fpu_present,
        reset_cycles,
        cyc_bcc_notake_b,
        cyc_bcc_notake_w,
        cyc_dbcc_f_noexp,
        cyc_dbcc_f_exp,
        cyc_scc_r_true,
        cyc_movem_w,
        cyc_movem_l,
        cyc_shift,
        cyc_reset,
        virq_state,
        nmi_pending,
        mmu_crp_aptr,
        mmu_crp_limit,
        mmu_srp_aptr,
        mmu_srp_limit,
        mmu_tc,
        mmu_sr,
        mmu_tt0,
        mmu_tt1,
        dacr0,
        dacr1,
        iacr0,
        iacr1,
        pcr,
        buscr,
        atc,
        atc_030,
        pending_fault_cause,
        mmu_fc_override,
        mmu_read_override,
        mmu_write_suppress,
        pending_fault_wdata,
        fault_resume,
        cycles_remaining,
        initial_cycles,
        fm_base,
        fm_len,
        trace_record_skip,
        pending_trap_resume,
        trace_probe_skip,
        trace_record_skip_at,
        trace_probe_skip_at,
        trace_recording,
        oep060,
        emulate_unimplemented_060,
        sst_m68000_compat,
    );
    fields.push(("window_present", (cpu.fm_ptr != 0).to_string()));
    fields.push(("bus_present", (cpu.fm_bus != 0).to_string()));
    fields.push(("tracked_hook_present", (cpu.fm_write_hook != 0).to_string()));
    fields
}

#[derive(Debug, PartialEq, Eq)]
enum Event {
    CpuView(Vec<(&'static str, String)>),
    Begin(u32, u32),
    Write(u32, u32, u32),
    End(Option<u32>),
}

struct TestBus {
    ram: Vec<u8>,
    events: Vec<Event>,
    tracked_mode: bool,
    // Used only by the raw-native oracle: no Rust CPU reference is alive during a hook.
    observer_cpu: *const CpuCore,
    observer_new_mask: Option<u32>,
    panic_read: Option<u32>,
}

impl AddressBus for TestBus {
    fn fast_mem(&mut self) -> Option<crate::FastMem> {
        (!self.tracked_mode).then(|| crate::FastMem {
            ptr: self.ram.as_mut_ptr(),
            base: 0,
            len: RAM_LEN as u32,
        })
    }
    fn read_byte(&mut self, address: u32) -> u8 {
        assert_ne!(
            self.panic_read,
            Some(address),
            "deliberate slow-validator bus panic"
        );
        self.ram.get(address as usize).copied().unwrap_or(0)
    }
    fn read_word(&mut self, address: u32) -> u16 {
        u16::from_be_bytes([
            self.read_byte(address),
            self.read_byte(address.wrapping_add(1)),
        ])
    }
    fn read_long(&mut self, address: u32) -> u32 {
        u32::from_be_bytes(std::array::from_fn(|i| {
            self.read_byte(address.wrapping_add(i as u32))
        }))
    }
    fn write_byte(&mut self, address: u32, value: u8) {
        self.events.push(Event::Write(address, 1, u32::from(value)));
        self.ram[address as usize] = value;
    }
    fn write_word(&mut self, address: u32, value: u16) {
        self.events.push(Event::Write(address, 2, u32::from(value)));
        self.ram[address as usize..address as usize + 2].copy_from_slice(&value.to_be_bytes());
    }
    fn write_long(&mut self, address: u32, value: u32) {
        self.events.push(Event::Write(address, 4, value));
        self.ram[address as usize..address as usize + 4].copy_from_slice(&value.to_be_bytes());
    }
    fn begin_memory_copy(&mut self, source: u32, bytes: u32) -> bool {
        self.events.push(Event::Begin(source, bytes));
        true
    }
    fn end_memory_copy(&mut self, destination: Option<u32>) {
        self.events.push(Event::End(destination));
    }
}

unsafe extern "C" fn write_hook(bus: usize, operation: u32, address: u32, value: u32, source: u32) {
    // SAFETY: Fixture owns a stable boxed bus and its RAM throughout every native invocation.
    let bus = unsafe { &mut *(bus as *mut TestBus) };
    if !bus.observer_cpu.is_null() {
        // SAFETY: Only the raw-entry test sets this pointer, to its stable boxed CPU.
        // Generated code is suspended in this synchronous hook; no Rust &mut CPU exists.
        bus.events
            .push(Event::CpuView(raw_state(unsafe { &*bus.observer_cpu })));
        if let Some(mask) = bus.observer_new_mask {
            // The same alias-free private oracle can test an unusual callback mutation.
            unsafe {
                (*(bus.observer_cpu as *mut CpuCore)).address_mask = mask;
            }
        }
    }
    let bytes = operation & 7;
    let copied = operation & 8 != 0 && bus.begin_memory_copy(source, bytes);
    match bytes {
        1 => bus.write_byte(address, value as u8),
        2 => bus.write_word(address, value as u16),
        4 => bus.write_long(address, value),
        _ => unreachable!("generated write width"),
    }
    if copied {
        bus.end_memory_copy(Some(address));
    }
}

struct Fixture {
    cpu: Box<CpuCore>,
    bus: Box<TestBus>,
    jit: TraceJit,
    root_cycles: i32,
    child_cycles: i32,
}

impl Fixture {
    fn new(enabled: bool, tracked: bool, copy_opcode: u16) -> Self {
        let mut bus = Box::new(TestBus {
            ram: vec![0; RAM_LEN],
            events: Vec::new(),
            tracked_mode: tracked,
            observer_cpu: std::ptr::null(),
            observer_new_mask: None,
            panic_read: None,
        });
        // A miniature version of the recorded sprite dispatcher. The table is read live.
        // CASE0 copies one operand; CASE1 is a read-only arm; CASE2 is register-only.
        let words = [
            0x1a1a,
            0x7000,
            0x1005,
            0xd040,
            0x303b,
            0x0006,
            0x4efb,
            0x0002,
            0x0008,
            0x000e,
            0x0014,
            // Table entry 3: CASE3. Commands 0..=2 never read it.
            (CASE3 - 0x2010) as u16,
            copy_opcode,
            0x5281,
            0x60e2,
            0x3413,
            0x5283,
            0x60dc,
            0x5286,
            0x5287,
            0x60d6,
            // CASE3: MOVE.W D5,(A5); ADDQ.L #1,D3; ADDQ.L #1,D6; BRA HEAD.
            0x3a85,
            0x5283,
            0x5286,
            0x60ce,
        ];
        for (i, word) in words.iter().enumerate() {
            bus.ram[HEAD as usize + i * 2..HEAD as usize + i * 2 + 2]
                .copy_from_slice(&word.to_be_bytes());
        }
        for i in 0..256 {
            bus.ram[SOURCE as usize + i] = (i as u8).wrapping_mul(37).wrapping_add(11);
            bus.ram[COMMANDS as usize + i] = [0, 1, 2][i % 3];
        }
        bus.ram[VALUE as usize..VALUE as usize + 2].copy_from_slice(&0x8123u16.to_be_bytes());
        let mut cpu = Box::new(CpuCore::new());
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = HEAD;
        cpu.set_a(2, COMMANDS);
        cpu.set_a(3, VALUE);
        cpu.set_a(4, SOURCE);
        cpu.set_a(5, DESTINATION);
        cpu.set_a(7, 0x7000);
        cpu.x_flag = 0x1ab;
        cpu.n_flag = 0x81;
        cpu.not_z_flag = 0x10203;
        cpu.v_flag = 0x86;
        cpu.c_flag = 0x1cd;
        cpu.cycles_remaining = 100_000;
        cpu.initial_cycles = 100_000;
        cpu.fm_ptr = bus.ram.as_mut_ptr() as usize;
        cpu.fm_base = 0;
        cpu.fm_len = RAM_LEN as u32;
        if tracked {
            cpu.fm_bus = (&raw mut *bus) as usize;
            cpu.fm_write_hook = write_hook as *const () as usize;
        }
        let mut jit = TraceJit::new();
        jit.native_region_enabled = enabled;
        let mut root_cycles = 0;
        let mut child_cycles = 0;
        for (head, pcs, end) in [
            (
                HEAD,
                vec![
                    0x2000,
                    0x2002,
                    0x2004,
                    0x2006,
                    0x2008,
                    0x200c,
                    CASE0,
                    CASE0 + 2,
                    CASE0 + 4,
                ],
                HEAD,
            ),
            (CASE1, vec![CASE1, CASE1 + 2, CASE1 + 4], HEAD),
            (CASE2, vec![CASE2, CASE2 + 2, CASE2 + 4], HEAD),
            (CASE3, vec![CASE3, CASE3 + 2, CASE3 + 4, CASE3 + 6], HEAD),
        ] {
            let mut ops: Vec<_> = pcs
                .into_iter()
                .map(|pc| {
                    decode_trace_op(&cpu, &mut *bus, pc, CpuType::M68040)
                        .expect("fixture instruction decodes")
                })
                .collect();
            for op in &mut ops {
                if let JitTraceOp::IndirectJmp {
                    expected_target, ..
                } = &mut op.op
                {
                    *expected_target = Some(CASE0);
                }
            }
            let mut trace = jit
                .compile_decoded_ops(&cpu, head, CpuType::M68040, ops, Some(end))
                .expect("fixture trace compiles");
            trace.adaptive_branch = false;
            if head == HEAD {
                root_cycles = trace.max_cycles;
            }
            if head == CASE1 {
                child_cycles = trace.max_cycles;
            }
            jit.slots[trace_cache_index(head)] = TraceSlot::Compiled(trace);
        }
        if enabled {
            jit.compile_native_region(&[HEAD, CASE1, CASE2])
                .expect("bounded fixture dispatcher compiles");
        }
        Self {
            cpu,
            bus,
            jit,
            root_cycles,
            child_cycles,
        }
    }

    fn run(&mut self, budget: u32, watches: &[u32], depth: u8) -> Option<(Option<u16>, u32)> {
        let cpu_type = self.cpu.cpu_type;
        let single_iter = watches.contains(&self.cpu.pc);
        self.jit
            .try_execute(
                &mut self.cpu,
                &mut *self.bus,
                cpu_type,
                budget,
                single_iter,
                watches,
                depth,
            )
            .map(|(result, retired)| {
                (
                    match result {
                        CachedRunResult::Ran => None,
                        CachedRunResult::Miss(op) => Some(op),
                    },
                    retired,
                )
            })
    }
}

fn admission_state(jit: &TraceJit) -> Vec<(usize, String)> {
    jit.slots.iter().enumerate().filter_map(|(index, slot)| {
        let state = match slot {
            TraceSlot::Empty => return None,
            TraceSlot::Counting { pc, cpu_type, hits, adaptive_rerecords, allow_call_through, deferred_trap, deferred_linear } => {
                format!("count {pc:x} {cpu_type:?} {hits} {adaptive_rerecords} {allow_call_through} {deferred_trap} {deferred_linear}")
            }
            TraceSlot::Rejected { pc, cpu_type } => format!("rejected {pc:x} {cpu_type:?}"),
            TraceSlot::Compiled(trace) => format!("compiled {:x} {:?} {} {} {}", trace.pc, trace.cpu_type, trace.adaptive_calls.get(), trace.adaptive_guard_exits.get(), trace.adaptive_rerecords),
        };
        Some((index, state))
    }).collect()
}

fn assert_equal(
    before: &Fixture,
    after: &Fixture,
    result_before: Option<(Option<u16>, u32)>,
    result_after: Option<(Option<u16>, u32)>,
    context: &str,
) {
    assert_eq!(
        result_after, result_before,
        "{context}: return/miss/retirement"
    );
    assert_eq!(
        raw_state(&after.cpu),
        raw_state(&before.cpu),
        "{context}: full raw CPU state"
    );
    assert_eq!(after.bus.ram, before.bus.ram, "{context}: all RAM");
    assert_eq!(
        after.bus.events, before.bus.events,
        "{context}: ordered copy/write callbacks"
    );
    assert_eq!(
        admission_state(&after.jit),
        admission_state(&before.jit),
        "{context}: candidacy and invalidation"
    );
    assert_eq!(
        after.jit.pending_exit_seed, before.jit.pending_exit_seed,
        "{context}: guard-exit provenance"
    );
}

fn compare(
    budget: u32,
    watches: &[u32],
    depth: u8,
    tracked: bool,
    copy_opcode: u16,
    configure: impl Fn(&mut Fixture),
) -> u64 {
    let mut before = Fixture::new(false, tracked, copy_opcode);
    let mut after = Fixture::new(true, tracked, copy_opcode);
    configure(&mut before);
    configure(&mut after);
    let result_before = before.run(budget, watches, depth);
    let result_after = after.run(budget, watches, depth);
    assert_equal(
        &before,
        &after,
        result_before,
        result_after,
        &format!("budget={budget} watches={watches:?} depth={depth} tracked={tracked}"),
    );
    assert_eq!(
        before.jit.native_region_entries, 0,
        "baseline must use the old driver"
    );
    after.jit.native_region_entries
}

#[test]
fn native_region_exact_instruction_budgets_and_chain_depths() {
    let mut entries = 0;
    for budget in (0..=19).chain([27, 28, 64]) {
        entries += compare(budget, &[], TRACE_EXIT_CHAIN_BUDGET, false, 0x1adc, |_| {});
    }
    for depth in 0..=TRACE_EXIT_CHAIN_BUDGET {
        entries += compare(100, &[], depth, false, 0x1adc, |_| {});
    }
    assert!(
        entries > 0,
        "comparison must actually enter the generated dispatcher"
    );
}

#[test]
fn native_region_cycle_limits_match_each_logical_head() {
    let mut entries = 0;
    for which in 0..8 {
        entries += compare(100, &[], TRACE_EXIT_CHAIN_BUDGET, false, 0x1adc, |f| {
            f.bus.ram[COMMANDS as usize] = 1;
            let a = f.root_cycles;
            let b = f.child_cycles;
            f.cpu.cycles_remaining =
                [0, 1, a - 1, a, a + b - 1, a + b, 2 * a + b - 1, 2 * a + b][which];
        });
    }
    assert!(entries > 0);
}

#[test]
fn native_region_watches_preserve_entry_interior_and_child_boundaries() {
    let mut entries = 0;
    for watches in [
        &[][..],
        &[HEAD],
        &[HEAD + 4],
        &[CASE1],
        &[CASE1 + 2],
        &[CASE2],
        &[0, CASE2 + 2],
    ] {
        entries += compare(100, watches, TRACE_EXIT_CHAIN_BUDGET, false, 0x1adc, |_| {});
    }
    assert!(entries > 0);
}

#[test]
fn native_region_keeps_ordered_byte_word_long_copy_notifications() {
    let mut entries = 0;
    for opcode in [0x1adc, 0x3adc, 0x2adc] {
        entries += compare(100, &[], TRACE_EXIT_CHAIN_BUDGET, true, opcode, |_| {});
    }
    assert!(
        entries > 0,
        "tracked stores must be covered by native execution"
    );
}

#[test]
fn native_region_partial_and_zero_retirement_memory_bails_match() {
    for address in [RAM_LEN as u32, RAM_LEN as u32 - 1] {
        compare(100, &[], TRACE_EXIT_CHAIN_BUDGET, true, 0x2adc, |f| {
            f.cpu.set_a(4, address);
        });
        compare(100, &[], TRACE_EXIT_CHAIN_BUDGET, true, 0x2adc, |f| {
            f.cpu.set_a(5, address);
        });
    }
    compare(100, &[], TRACE_EXIT_CHAIN_BUDGET, false, 0x1adc, |f| {
        f.cpu.set_a(2, RAM_LEN as u32);
    });
    compare(100, &[], TRACE_EXIT_CHAIN_BUDGET, false, 0x1adc, |f| {
        f.bus.ram[COMMANDS as usize] = 1;
        f.cpu.set_a(3, RAM_LEN as u32);
    });
}

#[test]
fn native_region_smc_preserves_parent_retirement_and_child_miss() {
    for address in [HEAD, HEAD + 2, CASE1, CASE1 + 2] {
        compare(100, &[], TRACE_EXIT_CHAIN_BUDGET, false, 0x1adc, |f| {
            f.bus.ram[COMMANDS as usize] = 1;
            f.bus.ram[address as usize..address as usize + 2]
                .copy_from_slice(&0x7055u16.to_be_bytes());
        });
    }
    // The root may legally write a future child's code; the child's checked entry must see it.
    compare(100, &[], TRACE_EXIT_CHAIN_BUDGET, true, 0x1adc, |f| {
        f.cpu.set_a(5, CASE1);
        f.bus.ram[SOURCE as usize] = 0x70;
    });
    // A raw guest-table rewrite is data, and must affect the indirect target immediately.
    compare(100, &[], TRACE_EXIT_CHAIN_BUDGET, false, 0x1adc, |f| {
        f.bus.ram[0x2010..0x2012].copy_from_slice(&0x0014u16.to_be_bytes());
    });
}

#[test]
fn native_region_changed_modes_fall_back_without_using_stale_entries() {
    for mode in 0..4 {
        compare(
            100,
            &[],
            TRACE_EXIT_CHAIN_BUDGET,
            false,
            0x1adc,
            |f| match mode {
                0 => f.cpu.set_cpu_type(CpuType::M68020),
                1 => {
                    f.cpu.has_pmmu = true;
                    f.cpu.pmmu_enabled = true;
                }
                2 => f.cpu.fm_len = 0,
                3 => {
                    f.cpu.fm_bus = (&raw mut *f.bus) as usize;
                    f.cpu.fm_write_hook = write_hook as *const () as usize;
                }
                _ => unreachable!(),
            },
        );
    }
}

#[test]
fn native_region_admits_adaptive_components_exactly() {
    // Adaptive re-record policy counts Rust entries; inside a region those
    // entries pause, but execution must stay identical to the old driver.
    let mark_adaptive = |f: &mut Fixture| {
        for pc in [HEAD, CASE1] {
            let TraceSlot::Compiled(trace) = &mut f.jit.slots[trace_cache_index(pc)] else {
                unreachable!()
            };
            trace.adaptive_branch = true;
        }
    };
    let mut f = Fixture::new(true, false, 0x1adc);
    mark_adaptive(&mut f);
    assert!(f.jit.compile_native_region(&[HEAD, CASE1, CASE2]).is_ok());
    let mut entered = 0;
    for budget in [1, 5, 9, 17, 40, 100] {
        let mut before = Fixture::new(false, false, 0x1adc);
        let mut after = Fixture::new(true, false, 0x1adc);
        mark_adaptive(&mut before);
        mark_adaptive(&mut after);
        let result_before = before.run(budget, &[], TRACE_EXIT_CHAIN_BUDGET);
        let result_after = after.run(budget, &[], TRACE_EXIT_CHAIN_BUDGET);
        let context = format!("budget={budget}");
        // Everything guest-visible is identical; only the adaptive policy's
        // Rust-entry counters differ, by design.
        assert_eq!(
            result_after, result_before,
            "{context}: return/miss/retirement"
        );
        assert_eq!(
            raw_state(&after.cpu),
            raw_state(&before.cpu),
            "{context}: CPU"
        );
        assert_eq!(after.bus.ram, before.bus.ram, "{context}: RAM");
        assert_eq!(after.bus.events, before.bus.events, "{context}: callbacks");
        entered += after.jit.native_region_entries;
    }
    assert!(entered > 0, "the region runs adaptive heads");
}

#[test]
fn native_region_unknown_target_is_counted_even_when_the_parent_uses_the_budget() {
    // Six instructions retire through the committed JMP. The generic driver must still
    // account candidacy of an unknown target before declining its zero-budget child entry.
    for budget in [6, 7, 9, 10] {
        compare(budget, &[], TRACE_EXIT_CHAIN_BUDGET, false, 0x1adc, |f| {
            // Record the valid six-op path that dispatches straight back to HEAD.
            // This places the guarded jump last, so budget=6 really enters and
            // retires its whole allowance before observing the unknown target.
            let TraceSlot::Compiled(old) = &f.jit.slots[trace_cache_index(HEAD)] else {
                unreachable!()
            };
            let mut ops = old.ops[..6].to_vec();
            let JitTraceOp::IndirectJmp {
                expected_target, ..
            } = &mut ops[5].op
            else {
                unreachable!()
            };
            *expected_target = Some(HEAD);
            let mut trace = f
                .jit
                .compile_decoded_ops(&f.cpu, HEAD, CpuType::M68040, ops, Some(HEAD))
                .unwrap();
            trace.adaptive_branch = false;
            f.jit.slots[trace_cache_index(HEAD)] = TraceSlot::Compiled(trace);
            if f.jit.native_region_enabled {
                f.jit.compile_native_region(&[HEAD, CASE1, CASE2]).unwrap();
            }
            f.bus.ram[0x2010..0x2012].copy_from_slice(&0x0120u16.to_be_bytes());
        });
    }
}

#[test]
fn native_region_smc_is_validated_before_a_small_nonzero_instruction_budget() {
    for budget in [1, 2, 6, 7, 8, 9] {
        compare(budget, &[], TRACE_EXIT_CHAIN_BUDGET, false, 0x1adc, |f| {
            f.bus.ram[COMMANDS as usize] = 1;
            f.bus.ram[(CASE1 + 2) as usize..(CASE1 + 4) as usize]
                .copy_from_slice(&0x7455u16.to_be_bytes());
        });
        compare(budget, &[], TRACE_EXIT_CHAIN_BUDGET, false, 0x1adc, |f| {
            f.bus.ram[(HEAD + 2) as usize..(HEAD + 4) as usize]
                .copy_from_slice(&0x7055u16.to_be_bytes());
        });
    }
}

#[test]
fn native_region_executes_multiple_heads_inside_the_generated_dispatcher() {
    let mut before = Fixture::new(false, true, 0x1adc);
    let mut after = Fixture::new(true, true, 0x1adc);
    assert!(
        after.jit.native_region_inlined_calls() >= 6,
        "all three checked entries and bodies must be inlined, not called through the old native ABI"
    );
    before.bus.ram[COMMANDS as usize] = 1;
    after.bus.ram[COMMANDS as usize] = 1;
    let a = before.run(100, &[], TRACE_EXIT_CHAIN_BUDGET);
    let b = after.run(100, &[], TRACE_EXIT_CHAIN_BUDGET);
    assert_equal(&before, &after, a, b, "actual native multi-head execution");
    assert!(
        after.jit.native_region_heads_run >= 2,
        "an entry immediately returning ResumeHead is not a successful dispatcher test"
    );
    assert_eq!(before.jit.native_region_heads_run, 0);
}

#[test]
fn native_region_mode_guards_do_not_execute_a_stale_native_component() {
    for tracked_mismatch in [false, true] {
        let mut f = Fixture::new(true, false, 0x1adc);
        if tracked_mismatch {
            f.cpu.fm_bus = (&raw mut *f.bus) as usize;
            f.cpu.fm_write_hook = write_hook as *const () as usize;
        } else {
            f.cpu.fm_len = 0;
        }
        f.run(100, &[], TRACE_EXIT_CHAIN_BUDGET);
        assert_eq!(
            f.jit.native_region_heads_run, 0,
            "entering guards and safely resuming is allowed; executing a stale component is not"
        );
    }
}

#[test]
fn native_region_is_promoted_by_the_real_recorder_and_used_by_run_batch() {
    struct RestoreJit(Option<TraceJit>);
    impl Drop for RestoreJit {
        fn drop(&mut self) {
            TRACE_JIT.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    fn run(enabled: bool) -> (Fixture, crate::BatchResult) {
        // All three arms are read-only. Begin with case0, then force the one allowed
        // adaptive rerecord onto case1 before mixing all three enough to earn the
        // existing 208-hit linear admission for both other arms.
        let mut f = Fixture::new(enabled, false, 0x5281);
        for i in 0..0x1000usize {
            f.bus.ram[COMMANDS as usize + i] = if i < 8 {
                0
            } else if i < 136 {
                1
            } else {
                (i % 3) as u8
            };
        }
        let mut fresh = TraceJit::new();
        fresh.native_region_enabled = enabled;
        let _restore = RestoreJit(TRACE_JIT.with(|slot| slot.replace(Some(fresh))));
        // None of Fixture's manually compiled traces are installed in this fresh TLS JIT.
        let result = f.cpu.run_batch(&mut *f.bus, 30_000, &[]);
        f.jit = TRACE_JIT.with(|slot| slot.borrow_mut().take().unwrap());
        (f, result)
    }
    let (before, a) = run(false);
    let (after, b) = run(true);
    assert_eq!(
        format!("{b:?}"),
        format!("{a:?}"),
        "public batch exit and retirement"
    );
    assert_eq!(b.instructions, 30_000);
    assert_eq!(
        raw_state(&after.cpu),
        raw_state(&before.cpu),
        "recorded-path raw CPU state"
    );
    assert_eq!(after.bus.ram, before.bus.ram, "recorded-path RAM");
    assert_eq!(after.bus.events, before.bus.events);
    assert!(
        after.jit.native_region_heads_run >= 2,
        "real recorder/admission must install and execute a multi-head dispatcher: {}",
        after.jit.native_region_status
    );
    assert_eq!(before.jit.native_region_heads_run, 0);
}

#[test]
fn native_region_callbacks_observe_the_same_raw_cpu_and_per_head_cycles() {
    let mut before = Fixture::new(false, true, 0x1adc);
    let mut after = Fixture::new(true, true, 0x1adc);
    for f in [&mut before, &mut after] {
        // Two copies with a read-only child between them expose per-head cycle charging.
        f.bus.ram[COMMANDS as usize..COMMANDS as usize + 4].copy_from_slice(&[0, 1, 0, 2]);
        f.bus.observer_cpu = &raw const *f.cpu;
    }
    let mut budget = 100u32;
    let mut depth = TRACE_EXIT_CHAIN_BUDGET;
    let mut total = 0;
    let mut calls = 0;
    let final_packed;
    // Alias-free form of the old eager driver's checked-call/account/chain sequence.
    // The ordinary differential tests above separately cover its Rust fallback logic.
    loop {
        let pc = before.cpu.pc;
        let TraceSlot::Compiled(trace) = &before.jit.slots[trace_cache_index(pc)] else {
            panic!("known fixture child")
        };
        let iterations = if trace.self_loop {
            (budget / trace.ops.len() as u32).min(
                (before
                    .cpu
                    .cycles_remaining
                    .min(TRACE_RETURN_CYCLES_MASK as i32)
                    / trace.max_cycles) as u32,
            )
        } else {
            1
        };
        assert!(iterations > 0);
        let cpu = &raw mut *before.cpu;
        // SAFETY: stable RAM/CPU/bus, current code, and sufficient per-head budgets.
        // No Rust CPU reference remains live across these raw native calls.
        let packed = unsafe {
            match trace.checked_func.unwrap() {
                NativeTraceFn::Once(entry) => entry(cpu),
                NativeTraceFn::Loop(entry) => entry(cpu, iterations),
            }
        };
        assert!(!trace_return_validation_needed(packed));
        let retired = trace_return_retired(packed);
        assert!(retired > 0);
        before.cpu.cycles_remaining -= trace_return_cycles(packed) as i32;
        total += retired;
        calls += 1;
        if depth == 0 {
            final_packed = packed;
            break;
        }
        assert_ne!(before.cpu.pc, pc);
        assert!(trace_return_complete(packed) || trace_return_guarded_branch_exit(packed));
        budget -= retired;
        depth -= 1;
    }
    // SAFETY: identical prepared snapshot, no watches, unchanged code/modes, eager state.
    let exit = unsafe {
        after
            .jit
            .call_native_region_raw(&raw mut *after.cpu, 100, TRACE_EXIT_CHAIN_BUDGET)
    };
    assert_eq!(
        exit.kind, 2,
        "known, budgeted four-head sequence finishes natively"
    );
    assert_eq!(exit.index, 2);
    assert_eq!(exit.instr_budget, budget);
    assert_eq!(exit.chain_budget, u32::from(depth));
    assert_eq!(exit.retired, total);
    assert_eq!(exit.latest_retired, trace_return_retired(final_packed));
    assert_eq!(exit.packed, final_packed);
    assert_eq!(calls, 4);
    let views = after
        .bus
        .events
        .iter()
        .filter(|event| matches!(event, Event::CpuView(_)))
        .count();
    assert_eq!(
        views, 2,
        "both sides of a logical head boundary must reach callbacks"
    );
    assert_eq!(
        after.bus.events, before.bus.events,
        "callback-visible raw flags, PC, registers, cycles, and ordered notifications"
    );
    assert_eq!(raw_state(&after.cpu), raw_state(&before.cpu));
    assert_eq!(after.bus.ram, before.bus.ram);
}

#[test]
fn native_region_callback_mask_change_returns_to_the_parent_host_boundary() {
    let mut f = Fixture::new(true, true, 0x1adc);
    f.bus.observer_cpu = &raw const *f.cpu;
    f.bus.observer_new_mask = Some(0x00ff_ffff);
    // SAFETY: same raw-entry callback oracle as above. The callback changes the mask
    // after a copy, before the root takes its guarded exit to CASE1.
    let exit = unsafe {
        f.jit
            .call_native_region_raw(&raw mut *f.cpu, 100, TRACE_EXIT_CHAIN_BUDGET)
    };
    assert_eq!(
        exit.kind, 2,
        "FINISH rechecks the exit watch; RESUME would bypass it"
    );
    assert_eq!(exit.index, 0);
    assert_eq!(exit.chain_budget, u32::from(TRACE_EXIT_CHAIN_BUDGET));
    assert_eq!(exit.retired, exit.latest_retired);
    assert_eq!(f.cpu.pc, CASE1);
    assert_eq!(f.cpu.address_mask, 0x00ff_ffff);
}

#[test]
fn native_region_slow_validator_panic_restores_enabled_state() {
    let mut before = Fixture::new(false, false, 0x1adc);
    let mut after = Fixture::new(true, false, 0x1adc);
    for f in [&mut before, &mut after] {
        f.bus.ram[COMMANDS as usize] = 1;
        f.bus.ram[CASE1 as usize..CASE1 as usize + 2].copy_from_slice(&0x7455u16.to_be_bytes());
        f.bus.panic_read = Some(CASE1);
    }
    let a = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        before.run(100, &[], TRACE_EXIT_CHAIN_BUDGET)
    }));
    let b = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        after.run(100, &[], TRACE_EXIT_CHAIN_BUDGET)
    }));
    assert!(
        a.is_err() && b.is_err(),
        "both slow validators must reach the injected bus panic"
    );
    assert!(
        after.jit.native_region_enabled,
        "temporary recursive fallback must be unwound"
    );
    assert!(!before.jit.native_region_enabled);
    assert_eq!(raw_state(&after.cpu), raw_state(&before.cpu));
    assert_eq!(after.bus.ram, before.bus.ram);
    assert_eq!(after.bus.events, before.bus.events);
}

/// Replace a fixture trace after modifying guest code. Both sides still compile the
/// same ordinary constituent, and only the enabled side enters the region.
fn replace_fixture_trace(f: &mut Fixture, pc: u32, ops: Vec<TraceBuildOp>) {
    let mut trace = f
        .jit
        .compile_decoded_ops(&f.cpu, pc, CpuType::M68040, ops, Some(HEAD))
        .expect("replacement fixture constituent compiles");
    trace.adaptive_branch = false;
    if pc == HEAD {
        f.root_cycles = trace.max_cycles;
    }
    if pc == CASE1 {
        f.child_cycles = trace.max_cycles;
    }
    f.jit.slots[trace_cache_index(pc)] = TraceSlot::Compiled(trace);
}

#[test]
fn native_region_condskip_retirement_survives_later_iterations_and_guard_exits() {
    for skip in [false, true] {
        for unknown in [false, true] {
            for budget in [8, 9, 17, 18, 26, 27, 28, 100] {
                let mut before = Fixture::new(false, false, 0x4e71);
                let mut after = Fixture::new(true, false, 0x4e71);
                for f in [&mut before, &mut after] {
                    // The live table load clears C and leaves Z clear. BNE therefore
                    // skips ADDQ while BEQ executes it. Two complete iterations then
                    // take the indirect guard: runtime retirement is not an op index.
                    let condition = if skip { 6 } else { 7 };
                    let opcode = if skip { 0x6602u16 } else { 0x6702u16 };
                    f.bus.ram[CASE0 as usize..CASE0 as usize + 2]
                        .copy_from_slice(&opcode.to_be_bytes());
                    let TraceSlot::Compiled(old) = &f.jit.slots[trace_cache_index(HEAD)] else {
                        unreachable!()
                    };
                    let mut ops = old.ops.clone();
                    ops[6] = TraceBuildOp {
                        pc: CASE0,
                        opcode,
                        extension: None,
                        extension2: None,
                        op: JitTraceOp::CondSkip {
                            condition,
                            skip_ops: 1,
                            length: 2,
                        },
                    };
                    replace_fixture_trace(f, HEAD, ops);
                    if f.jit.native_region_enabled {
                        f.jit.compile_native_region(&[HEAD, CASE1, CASE2]).unwrap();
                    }
                    f.bus.ram[COMMANDS as usize..COMMANDS as usize + 3].copy_from_slice(&[
                        0,
                        0,
                        if unknown { 3 } else { 1 },
                    ]);
                    f.bus.ram[0x2016..0x2018].copy_from_slice(&0x0120u16.to_be_bytes());
                }
                let a = before.run(budget, &[], TRACE_EXIT_CHAIN_BUDGET);
                let b = after.run(budget, &[], TRACE_EXIT_CHAIN_BUDGET);
                assert_equal(
                    &before,
                    &after,
                    a,
                    b,
                    &format!("dynamic retirement skip={skip} unknown={unknown} budget={budget}"),
                );
                if budget == 27 {
                    let root_retired = if skip { 22 } else { 24 };
                    let expected = root_retired + if unknown { 0 } else { 3 };
                    assert_eq!(
                        b,
                        Some((None, expected)),
                        "two full iterations plus partial indirect exit, then optional child"
                    );
                    assert!(after.jit.native_region_heads_run >= if unknown { 1 } else { 2 });
                }
            }
        }
    }
}

#[test]
fn native_region_root_stores_commit_before_child_code_is_revalidated() {
    for tracked in [false, true] {
        for location in 0..3 {
            let mut before = Fixture::new(false, tracked, 0x3adc);
            let mut after = Fixture::new(true, tracked, 0x3adc);
            let child = if location == 2 { 0x2070u32 } else { CASE1 };
            let destination = if location == 0 { child } else { child + 2 };
            let changed = if location == 2 { 0x0004u16 } else { 0x7455u16 };
            for f in [&mut before, &mut after] {
                if location == 2 {
                    // MOVE.W d16(A3),D2; ADDQ.L #1,D3; BRA HEAD. Three ops retain
                    // ordinary linear admission. Place the longer arm separately
                    // so its extension cannot overlap the existing CASE2 code.
                    f.bus.ram[child as usize..child as usize + 8]
                        .copy_from_slice(&[0x34, 0x2b, 0x00, 0x00, 0x52, 0x83, 0x60, 0x88]);
                    f.bus.ram[0x2012..0x2014].copy_from_slice(&0x0060u16.to_be_bytes());
                    let ops = [child, child + 4, child + 6]
                        .into_iter()
                        .map(|pc| {
                            decode_trace_op(&f.cpu, &mut *f.bus, pc, CpuType::M68040).unwrap()
                        })
                        .collect();
                    replace_fixture_trace(f, child, ops);
                    if f.jit.native_region_enabled {
                        f.jit.compile_native_region(&[HEAD, child, CASE2]).unwrap();
                    }
                }
                f.cpu.set_a(5, destination);
                f.bus.ram[SOURCE as usize..SOURCE as usize + 2]
                    .copy_from_slice(&changed.to_be_bytes());
                f.bus.ram[COMMANDS as usize..COMMANDS as usize + 2].copy_from_slice(&[0, 1]);
            }
            let a = before.run(100, &[], TRACE_EXIT_CHAIN_BUDGET);
            let b = after.run(100, &[], TRACE_EXIT_CHAIN_BUDGET);
            assert_equal(
                &before,
                &after,
                a,
                b,
                &format!("child code store tracked={tracked} location={location}"),
            );
            assert_eq!(
                &after.bus.ram[destination as usize..destination as usize + 2],
                &changed.to_be_bytes(),
                "the old root permits this write; a region-wide SMC barrier must not move its bail earlier"
            );
            assert_eq!(after.cpu.a(4), SOURCE + 2);
            assert_eq!(after.cpu.a(5), destination + 2);
            assert!(
                after.jit.native_region_heads_run > 0,
                "the root store must execute in the region"
            );
            assert!(
                matches!(&after.jit.slots[trace_cache_index(child)], TraceSlot::Empty),
                "the child's changed opcode/interior/extension must invalidate its compiled slot"
            );
            if tracked {
                assert_eq!(
                    after.bus.events,
                    vec![
                        Event::Begin(SOURCE, 2),
                        Event::Write(destination, 2, u32::from(changed)),
                        Event::End(Some(destination))
                    ]
                );
            }
        }
    }
}

#[test]
fn native_region_rechecks_code_and_live_dispatch_table_on_later_invocations() {
    for change in 0..3 {
        let mut before = Fixture::new(false, true, 0x1adc);
        let mut after = Fixture::new(true, true, 0x1adc);
        let a = before.run(100, &[], TRACE_EXIT_CHAIN_BUDGET);
        let b = after.run(100, &[], TRACE_EXIT_CHAIN_BUDGET);
        assert_equal(&before, &after, a, b, "first invocation before mutation");
        assert_eq!(after.cpu.pc, HEAD);
        assert!(after.jit.native_region_heads_run >= 2);
        for f in [&mut before, &mut after] {
            f.bus.ram[f.cpu.a(2) as usize] = if change == 1 { 2 } else { 0 };
            match change {
                0 => f.bus.ram[(HEAD + 2) as usize..(HEAD + 4) as usize]
                    .copy_from_slice(&0x7001u16.to_be_bytes()),
                1 => f.bus.ram[(CASE2 + 2) as usize..(CASE2 + 4) as usize]
                    .copy_from_slice(&0x7e55u16.to_be_bytes()),
                2 => f.bus.ram[0x2010..0x2012].copy_from_slice(&0x0120u16.to_be_bytes()),
                _ => unreachable!(),
            }
        }
        let a = before.run(100, &[], TRACE_EXIT_CHAIN_BUDGET);
        let b = after.run(100, &[], TRACE_EXIT_CHAIN_BUDGET);
        assert_equal(
            &before,
            &after,
            a,
            b,
            &format!("second invocation after mutation {change}"),
        );
        if change == 2 {
            assert_eq!(
                after.cpu.pc, 0x2130,
                "the indirect table remains live guest data"
            );
        }
    }
}

#[test]
fn run_batch_never_installs_a_window_aliasing_the_cpu() {
    let mut before = Fixture::new(false, false, 0x2adc);
    let mut after = Fixture::new(true, false, 0x2adc);
    for f in [&mut before, &mut after] {
        let code_offset = std::mem::offset_of!(CpuCore, dar_save);
        let guest_base = HEAD.checked_sub(code_offset as u32).unwrap();
        let guest_field = |offset: usize| guest_base + offset as u32;
        let cpu = (&raw mut *f.cpu).cast::<u8>();
        let code_len = (CASE2 + 6 - HEAD) as usize;
        assert!(code_len <= std::mem::size_of_val(&f.cpu.dar_save));
        // SAFETY: dar_save is a u32 array, so these instruction bytes create no
        // invalid Rust values. It is unused by this raw native path. The code and
        // data window are wholly inside this stable boxed CPU allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(
                f.bus.ram.as_ptr().add(HEAD as usize),
                cpu.add(code_offset),
                code_len,
            );
        }
        f.cpu.fm_ptr = cpu as usize;
        f.cpu.fm_base = guest_base;
        f.cpu.fm_len = std::mem::size_of::<CpuCore>() as u32;
        // The first command is zero, selecting the root's copy arm. That arm reads
        // D0's backing bytes after the live table lookup has stored 8 into D0.
        f.cpu.mmu_crp_aptr = 0;
        f.cpu.mmu_srp_aptr = 0xfeed_beef;
        f.cpu.set_d(0, 0xabcd_1234);
        f.cpu
            .set_a(2, guest_field(std::mem::offset_of!(CpuCore, mmu_crp_aptr)));
        f.cpu
            .set_a(4, guest_field(std::mem::offset_of!(CpuCore, dar)));
        f.cpu
            .set_a(5, guest_field(std::mem::offset_of!(CpuCore, mmu_srp_aptr)));
    }
    // Generated bodies keep guest registers and flags in native values between
    // entry and exit, so such a window can never be installed: run_batch
    // withholds it and the interpreter executes these instructions exactly.
    assert!(before.cpu.window_overlaps_cpu());
    assert!(after.cpu.window_overlaps_cpu());
    let plain = Fixture::new(true, false, 0x2adc);
    assert!(!plain.cpu.window_overlaps_cpu());
}

#[test]
fn hot_edge_region_with_a_fourth_memory_head_matches_every_budget() {
    // Commands mix all four cases so the region's fourth head (index 3)
    // runs, exits back to the root, and is cut by the budget at each point.
    let configure = |f: &mut Fixture| {
        for i in 0..256 {
            f.bus.ram[COMMANDS as usize + i] = [3, 1, 3, 2, 0, 3, 3, 1][i % 8];
        }
        if f.jit.native_region_enabled {
            for _ in 0..native_region::HOT_REGION_EDGE {
                f.jit.note_region_edge(CASE3, HEAD);
            }
            f.jit
                .compile_native_region(&[HEAD, CASE1, CASE2, CASE3])
                .expect("hot memory arm joins the region");
            assert_eq!(f.jit.native_region_inlined_calls(), 8, "four inlined heads");
        }
    };
    for tracked in [false, true] {
        let mut entered = 0;
        for budget in (1..=40).chain([63, 64, 65, 100, 257]) {
            entered += compare(
                budget,
                &[],
                TRACE_EXIT_CHAIN_BUDGET,
                tracked,
                0x1adc,
                configure,
            );
        }
        assert!(entered > 0, "tracked={tracked}: the four-head region ran");
    }
}
