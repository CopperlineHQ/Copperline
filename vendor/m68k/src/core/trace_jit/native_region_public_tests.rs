//! Shared public run_batch oracle. Region shape, private returns and synthetic cycles
//! are deliberately not compared. Every public return is compared before resuming.
#![cfg(all(
    feature = "jit",
    not(target_family = "wasm"),
    not(feature = "trace-profile")
))]
use super::*;
use crate::core::memory::{BusFault, BusFaultKind};
use crate::{AddressBus, BatchExit, BatchResult, FastMem, TrackedMem};

const HEAD: u32 = 0x2000;
const COPY: u32 = 0x2018;
const ARM: u32 = 0x201e;
const ARM2: u32 = 0x2024;
const EXT_ARM: u32 = 0x2070;
const TRAP: u32 = 0x2100;
const HANDLER: u32 = 0x2800;
const SOURCE: u32 = 0x3000;
const DEST: u32 = 0x4000;
const COMMANDS: u32 = 0x5000;
const VALUE: u32 = 0x6000;
const LEN: usize = 0x8000;

/// Architectural/control/exception state, including raw flag representations.
/// Omitted: allocation identities, decode/trace caches, rollback scratch dar_save/
/// sr_save, prefetch/timing bookkeeping and the documented clobbered batch cycles.
/// No register, PC/PPC/IR, flag, stack, interrupt, MMU or exception field is normalized.
fn architecture(cpu: &CpuCore) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    macro_rules! fields { ($($f:ident),* $(,)?) => { $(out.push((stringify!($f), format!("{:?}", cpu.$f)));)* }; }
    fields!(
        dar,
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
        pending_fault_cause,
        mmu_fc_override,
        mmu_read_override,
        mmu_write_suppress,
        pending_fault_wdata,
        fault_resume,
        emulate_unimplemented_060,
        sst_m68000_compat
    );
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Begin {
        source: u32,
        bytes: u32,
        snapshot: Vec<u8>,
    },
    Write {
        address: u32,
        bytes: u32,
        value: u32,
        before: Vec<u8>,
        after: Vec<u8>,
        protected: bool,
    },
    End {
        destination: Option<u32>,
        snapshot: Vec<u8>,
    },
}

struct Bus {
    ram: Vec<u8>,
    tracked: bool,
    window_len: usize,
    protect: Option<(u32, u32)>,
    events: Vec<Event>,
    pending_copy: Option<u32>,
}
impl Bus {
    fn bytes(&self, address: u32, n: u32) -> Vec<u8> {
        (0..n)
            .map(|i| self.ram.get((address + i) as usize).copied().unwrap_or(0))
            .collect()
    }
    fn put(&mut self, address: u32, words: &[u16]) {
        for (i, word) in words.iter().enumerate() {
            self.ram[address as usize + i * 2..address as usize + i * 2 + 2]
                .copy_from_slice(&word.to_be_bytes());
        }
    }
    fn check(&self, a: u32, n: usize) -> Result<(), BusFault> {
        if (a as usize)
            .checked_add(n)
            .is_some_and(|end| end <= self.ram.len())
        {
            Ok(())
        } else {
            Err(BusFault {
                kind: BusFaultKind::BusError,
                address: a,
            })
        }
    }
    fn store(&mut self, a: u32, value: u32, n: u32) {
        let before = self.bytes(a, n);
        let protected = self
            .protect
            .is_some_and(|(start, end)| a < end && a + n > start);
        if !protected {
            for i in 0..n {
                self.ram[(a + i) as usize] = (value >> ((n - i - 1) * 8)) as u8;
            }
        }
        if self.tracked {
            self.events.push(Event::Write {
                address: a,
                bytes: n,
                value,
                before,
                after: self.bytes(a, n),
                protected,
            });
        }
    }
}
impl AddressBus for Bus {
    fn fast_mem(&mut self) -> Option<FastMem> {
        (!self.tracked).then(|| FastMem {
            ptr: self.ram.as_mut_ptr(),
            base: 0,
            len: self.window_len as u32,
        })
    }
    fn tracked_mem(&mut self) -> Option<TrackedMem> {
        self.tracked.then(|| TrackedMem {
            ptr: self.ram.as_ptr(),
            base: 0,
            len: self.window_len as u32,
        })
    }
    fn read_byte(&mut self, a: u32) -> u8 {
        self.ram.get(a as usize).copied().unwrap_or(0)
    }
    fn read_word(&mut self, a: u32) -> u16 {
        u16::from_be_bytes(self.bytes(a, 2).try_into().unwrap())
    }
    fn read_long(&mut self, a: u32) -> u32 {
        u32::from_be_bytes(self.bytes(a, 4).try_into().unwrap())
    }
    fn write_byte(&mut self, a: u32, v: u8) {
        self.store(a, u32::from(v), 1);
    }
    fn write_word(&mut self, a: u32, v: u16) {
        self.store(a, u32::from(v), 2);
    }
    fn write_long(&mut self, a: u32, v: u32) {
        self.store(a, v, 4);
    }
    fn try_read_byte(&mut self, a: u32) -> Result<u8, BusFault> {
        self.check(a, 1)?;
        Ok(self.read_byte(a))
    }
    fn try_read_word(&mut self, a: u32) -> Result<u16, BusFault> {
        self.check(a, 2)?;
        Ok(self.read_word(a))
    }
    fn try_read_long(&mut self, a: u32) -> Result<u32, BusFault> {
        self.check(a, 4)?;
        Ok(self.read_long(a))
    }
    fn try_write_byte(&mut self, a: u32, v: u8) -> Result<(), BusFault> {
        self.check(a, 1)?;
        self.write_byte(a, v);
        Ok(())
    }
    fn try_write_word(&mut self, a: u32, v: u16) -> Result<(), BusFault> {
        self.check(a, 2)?;
        self.write_word(a, v);
        Ok(())
    }
    fn try_write_long(&mut self, a: u32, v: u32) -> Result<(), BusFault> {
        self.check(a, 4)?;
        self.write_long(a, v);
        Ok(())
    }
    fn begin_memory_copy(&mut self, a: u32, n: u32) -> bool {
        if !self.tracked {
            return false;
        }
        assert!(
            self.pending_copy.replace(n).is_none(),
            "copy callbacks must not nest"
        );
        self.events.push(Event::Begin {
            source: a,
            bytes: n,
            snapshot: self.bytes(a, n),
        });
        true
    }
    fn end_memory_copy(&mut self, destination: Option<u32>) {
        let n = self.pending_copy.take().expect("end has matching begin");
        self.events.push(Event::End {
            destination,
            snapshot: destination.map(|a| self.bytes(a, n)).unwrap_or_default(),
        });
    }
}

// Adapt only these three functions when sharing the module with another emitter.
fn new_candidate_jit(enabled: bool) -> TraceJit {
    let mode = if enabled {
        native_region::RegionMode::Public
    } else {
        native_region::RegionMode::Disabled
    };
    TraceJit::new_with_region_mode(mode)
}
fn configure_candidate(jit: &mut TraceJit, enabled: bool, heads: &[u32]) {
    jit.native_region_enabled = enabled;
    if enabled {
        jit.compile_native_region(heads)
            .expect("public region admitted");
    } else {
        for &head in heads {
            let TraceSlot::Compiled(trace) = &jit.slots[trace_cache_index(head)] else {
                panic!("ordinary head remains installed");
            };
            assert!(
                trace.region_ir.is_none(),
                "Disabled must not retain region IR or pay capture overhead"
            );
        }
    }
}
fn candidate_entries(jit: &TraceJit) -> u64 {
    jit.native_region_public_entries
}

struct Fixture {
    cpu: CpuCore,
    bus: Bus,
    jit: Option<TraceJit>,
    enabled: bool,
}

#[test]
fn native_region_worker_keeps_running_old_code_then_publishes_the_real_region() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    // Always release the held compiler if a public-state assertion panics.
    struct Release(mpsc::Sender<()>);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let mut before = Fixture::new(false, true, 4, false);
    let mut after = Fixture::new(true, true, 4, false);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Release(release_tx);
    let jit = after.jit.as_mut().unwrap();
    jit.native_region = None;
    jit.enable_native_region_worker_with_barrier(started_tx, release_rx);
    jit.maybe_compile_native_region(HEAD);
    started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(jit.native_region_worker.as_ref().unwrap().pending());
    assert!(jit.native_region.is_none());

    // The worker cannot finish yet. Public batches must still execute exactly
    // the same instructions and tracked copies through the existing tier.
    for budget in [1, 7, 11, 23, 64, 500] {
        paired_batch(&mut before, &mut after, budget, &[]);
        assert_eq!(candidate_entries(after.jit.as_ref().unwrap()), 0);
        assert!(after.jit.as_ref().unwrap().native_region.is_none());
    }
    drop(release);
    let deadline = Instant::now() + Duration::from_secs(10);
    while after.jit.as_ref().unwrap().native_region.is_none() {
        assert!(
            Instant::now() < deadline,
            "real worker compilation/publication timed out"
        );
        after.jit.as_mut().unwrap().poll_native_region(HEAD);
        std::thread::yield_now();
    }
    assert!(after.jit.as_ref().unwrap().native_region_inlined_calls() >= 6);
    for budget in [1, 7, 11, 23, 64, 500] {
        paired_batch(&mut before, &mut after, budget, &[]);
    }
    assert!(candidate_entries(after.jit.as_ref().unwrap()) > 0);
}

#[test]
fn native_region_worker_rejects_stale_code_and_compiles_the_replacement() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    struct Release(mpsc::Sender<()>);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let mut before = Fixture::new(false, true, 4, false);
    let mut after = Fixture::new(true, true, 4, false);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Release(release_tx);
    let jit = after.jit.as_mut().unwrap();
    jit.native_region = None;
    jit.enable_native_region_worker_with_barrier(started_tx, release_rx);
    jit.maybe_compile_native_region(HEAD);
    started_rx.recv_timeout(Duration::from_secs(10)).unwrap();

    // Change an arm while its original immutable snapshot is held by the
    // worker. The replacement has different semantics and native identities.
    for f in [&mut before, &mut after] {
        f.bus.put(ARM + 2, &[0x5483]); // ADDQ.L #2,D3, formerly #1
        let ops = [ARM, ARM + 2, ARM + 4]
            .into_iter()
            .map(|pc| decode_trace_op(&f.cpu, &mut f.bus, pc, CpuType::M68040).unwrap())
            .collect();
        f.cpu.fm_ptr = f.bus.ram.as_mut_ptr() as usize;
        f.cpu.fm_len = LEN as u32;
        f.cpu.fm_write_hook = 1;
        let jit = f.jit.as_mut().unwrap();
        let mut trace = jit
            .compile_decoded_ops(&f.cpu, ARM, CpuType::M68040, ops, Some(HEAD))
            .unwrap();
        trace.adaptive_branch = false;
        jit.slots[trace_cache_index(ARM)] = TraceSlot::Compiled(trace);
        jit.maybe_compile_native_region(ARM);
        f.cpu.fm_ptr = 0;
        f.cpu.fm_len = 0;
        f.cpu.fm_write_hook = 0;
    }
    for budget in [1, 7, 11, 23, 64, 500] {
        paired_batch(&mut before, &mut after, budget, &[]);
        assert_eq!(candidate_entries(after.jit.as_ref().unwrap()), 0);
    }
    release.0.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "stale result did not rediscover replacement"
        );
        let jit = after.jit.as_mut().unwrap();
        jit.poll_native_region(HEAD);
        // The first result must never be published, even briefly. Rediscovery
        // submits the replacement without a new trace-compilation event.
        assert!(jit.native_region.is_none());
        if started_rx.try_recv().is_ok() {
            break;
        }
        std::thread::yield_now();
    }
    drop(release); // Allow the replacement to compile only after stale rejection.
    while after.jit.as_ref().unwrap().native_region.is_none() {
        assert!(
            Instant::now() < deadline,
            "replacement publication timed out"
        );
        after.jit.as_mut().unwrap().poll_native_region(HEAD);
        std::thread::yield_now();
    }
    assert!(after.jit.as_ref().unwrap().native_region_inlined_calls() >= 6);
    for budget in [1, 7, 11, 23, 64, 500] {
        paired_batch(&mut before, &mut after, budget, &[]);
    }
    assert!(candidate_entries(after.jit.as_ref().unwrap()) > 0);
}

impl Fixture {
    fn new(enabled: bool, tracked: bool, width: u32, extension_arm: bool) -> Self {
        let mut bus = Bus {
            ram: vec![0; LEN],
            tracked,
            window_len: LEN,
            protect: None,
            events: Vec::new(),
            pending_copy: None,
        };
        let copy = match width {
            1 => 0x1adc,
            2 => 0x3adc,
            4 => 0x2adc,
            _ => unreachable!(),
        };
        bus.put(
            HEAD,
            &[
                0x1a1a,
                0x7000,
                0x1005,
                0xd040,
                0x303b,
                0x0006,
                0x4efb,
                0x0002,
                0x0008,
                if extension_arm { 0x0060 } else { 0x000e },
                0x0014,
                0x00f0,
                copy,
                0x5281,
                0x60e2,
                0x3413,
                0x5283,
                0x60dc,
                0x5286,
                0x5287,
                0x60d6,
            ],
        );
        bus.put(EXT_ARM, &[0x342b, 0x0000, 0x5283, 0x6088]);
        bus.put(TRAP, &[0xa123, 0x4e71, 0x4e72, 0x2700]);
        bus.put(HANDLER, &[0x7e55, 0xa321]);
        for vector in [2u32, 3, 9, 31] {
            bus.ram[(vector * 4) as usize..(vector * 4 + 4) as usize]
                .copy_from_slice(&HANDLER.to_be_bytes());
        }
        for i in 0..0x1000usize {
            bus.ram[COMMANDS as usize + i] = [0, 1, 2][i % 3];
            bus.ram[SOURCE as usize + i] = (i as u8).wrapping_mul(17).wrapping_add(5);
        }
        bus.put(VALUE, &[0x8123, 0x4567, 0x89ab]);
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = HEAD;
        cpu.set_a(2, COMMANDS);
        cpu.set_a(3, VALUE);
        cpu.set_a(4, SOURCE);
        cpu.set_a(5, DEST);
        cpu.set_a(7, 0x7000);
        cpu.fm_ptr = bus.ram.as_mut_ptr() as usize;
        cpu.fm_base = 0;
        cpu.fm_len = LEN as u32;
        // Compilation observes only whether a tracked hook exists; no generated code
        // runs before run_batch installs its real, typed tracked-write trampoline.
        cpu.fm_write_hook = usize::from(tracked);
        let arm = if extension_arm { EXT_ARM } else { ARM };
        let mut jit = new_candidate_jit(enabled);
        for (pc, pcs) in [
            (
                HEAD,
                vec![
                    HEAD,
                    HEAD + 2,
                    HEAD + 4,
                    HEAD + 6,
                    HEAD + 8,
                    HEAD + 12,
                    COPY,
                    COPY + 2,
                    COPY + 4,
                ],
            ),
            (
                arm,
                if extension_arm {
                    vec![arm, arm + 4, arm + 6]
                } else {
                    vec![arm, arm + 2, arm + 4]
                },
            ),
            (ARM2, vec![ARM2, ARM2 + 2, ARM2 + 4]),
        ] {
            let mut ops: Vec<_> = pcs
                .into_iter()
                .map(|p| decode_trace_op(&cpu, &mut bus, p, CpuType::M68040).unwrap())
                .collect();
            for op in &mut ops {
                if let JitTraceOp::IndirectJmp {
                    expected_target, ..
                } = &mut op.op
                {
                    *expected_target = Some(COPY);
                }
            }
            let mut trace = jit
                .compile_decoded_ops(&cpu, pc, CpuType::M68040, ops, Some(HEAD))
                .expect("ordinary released constituent admitted");
            trace.adaptive_branch = false;
            jit.slots[trace_cache_index(pc)] = TraceSlot::Compiled(trace);
        }
        configure_candidate(&mut jit, enabled, &[HEAD, arm, ARM2]);
        // Manual installation must publish the same monotonic hint that normal
        // candidacy would set; otherwise early budget/SMC cases can bypass all JITs.
        TRACE_JIT_HAS_CANDIDATES.store(true, Ordering::Relaxed);
        cpu.fm_ptr = 0;
        cpu.fm_len = 0;
        cpu.fm_write_hook = 0;
        Self {
            cpu,
            bus,
            jit: Some(jit),
            enabled,
        }
    }
    fn in_jit<R>(&mut self, f: impl FnOnce(&mut CpuCore, &mut Bus) -> R) -> R {
        struct Restore(Option<TraceJit>);
        impl Drop for Restore {
            fn drop(&mut self) {
                TRACE_JIT.with(|slot| *slot.borrow_mut() = self.0.take());
            }
        }
        let restore = Restore(TRACE_JIT.with(|slot| slot.replace(self.jit.take())));
        let result = f(&mut self.cpu, &mut self.bus);
        self.jit = TRACE_JIT.with(|slot| slot.borrow_mut().take());
        drop(restore);
        result
    }
    fn batch(&mut self, budget: u32, watches: &[u32]) -> BatchResult {
        self.in_jit(|cpu, bus| cpu.run_batch(bus, budget, watches))
    }
}

fn compare_boundary(
    before: &Fixture,
    after: &Fixture,
    a: BatchResult,
    b: BatchResult,
    label: &str,
) {
    assert_eq!(b, a, "{label}: public retirement and exit");
    assert_eq!(
        architecture(&after.cpu),
        architecture(&before.cpu),
        "{label}: architectural state"
    );
    assert_eq!(
        after.bus.ram, before.bus.ram,
        "{label}: guest RAM including exception frames"
    );
    assert_eq!(
        after.bus.events, before.bus.events,
        "{label}: ordered copy/write/protection effects"
    );
    assert_eq!(
        after.bus.pending_copy, None,
        "{label}: notification completed before return"
    );
}
fn paired_batch(
    before: &mut Fixture,
    after: &mut Fixture,
    budget: u32,
    watches: &[u32],
) -> BatchResult {
    let a = before.batch(budget, watches);
    let b = after.batch(budget, watches);
    compare_boundary(
        before,
        after,
        a,
        b,
        &format!("budget={budget}, watches={watches:x?}"),
    );
    assert!(b.instructions <= budget);
    b
}

#[test]
fn public_region_matches_every_budget_and_changed_watch_boundary() {
    for tracked in [false, true] {
        for width in [1, 2, 4] {
            let mut before = Fixture::new(false, tracked, width, false);
            let mut after = Fixture::new(true, tracked, width, false);
            for budget in [0, 1, 2, 3, 5, 6, 7, 8, 9, 15, 16, 17, 31, 64, 257] {
                let result = paired_batch(&mut before, &mut after, budget, &[]);
                assert_eq!(result.instructions, budget);
                assert_eq!(result.exit, BatchExit::BudgetExhausted);
            }
            assert!(
                candidate_entries(after.jit.as_ref().unwrap()) > 0,
                "candidate must actually execute"
            );
            for pc in [
                HEAD,
                HEAD + 2,
                HEAD + 4,
                HEAD + 6,
                HEAD + 8,
                HEAD + 12,
                COPY,
                COPY + 2,
                COPY + 4,
                ARM,
                ARM + 2,
                ARM + 4,
                ARM2,
                ARM2 + 2,
                ARM2 + 4,
            ] {
                // Reaching the current PC is observed only after leaving/reentering it.
                paired_batch(&mut before, &mut after, 41, &[pc]);
                paired_batch(&mut before, &mut after, 1, &[pc]);
            }
        }
    }
}

#[test]
fn public_region_smc_store_has_exact_effects_at_budget_and_watch_cuts() {
    for (width, location) in [(1, 0), (2, 1), (2, 2), (4, 3), (2, 4), (2, 5)] {
        for protected in [false, true] {
            for first_budget in [6, 7, 15] {
                for watch_kind in 0..3 {
                    let extension = location == 2 || location == 5;
                    let mut before = Fixture::new(false, true, width, extension);
                    let mut after = Fixture::new(true, true, width, extension);
                    let child = if extension { EXT_ARM } else { ARM };
                    let destination = match location {
                        0 => child,
                        1 => child + 2,
                        2 => child + 2,
                        3 => ARM2,
                        4 => HEAD + 2,
                        5 => 0x2060,
                        _ => unreachable!(),
                    };
                    for f in [&mut before, &mut after] {
                        f.cpu.set_a(5, destination);
                        let value: &[u8] = match location {
                            0 => &[0x74],
                            1 => &[0x76, 0x55],
                            2 => &[0x00, 0x04],
                            3 => &[0x7c, 0x11, 0x7e, 0x22],
                            4 => &[0x70, 0x01],
                            5 => &[0x12, 0x34],
                            _ => unreachable!(),
                        };
                        f.bus.ram[SOURCE as usize..SOURCE as usize + width as usize]
                            .copy_from_slice(value);
                        f.bus.ram[COMMANDS as usize..COMMANDS as usize + 4]
                            .copy_from_slice(&[0, 1, 3, 3]);
                        if protected {
                            f.bus.protect = Some((destination, destination + width));
                        }
                    }
                    let initial_watch = match watch_kind {
                        0 => vec![],
                        1 => vec![COPY],
                        2 => vec![child],
                        _ => unreachable!(),
                    };
                    paired_batch(&mut before, &mut after, first_budget, &initial_watch);
                    for (budget, watches) in [
                        (0, vec![]),
                        (1, vec![]),
                        (31, vec![COPY]),
                        (31, vec![child]),
                        (31, vec![TRAP]),
                    ] {
                        paired_batch(&mut before, &mut after, budget, &watches);
                        if after.cpu.pc >= TRAP && after.cpu.pc <= TRAP + 2 {
                            break;
                        }
                    }
                    let writes: Vec<_> = after.bus.events.iter().filter(|e| matches!(e, Event::Write { address, .. } if *address == destination)).collect();
                    assert_eq!(
                        writes.len(),
                        1,
                        "SMC fallback must neither omit nor repeat its store"
                    );
                    assert!(
                        matches!(writes[0], Event::Write { bytes, protected: p, .. } if *bytes == width && *p == protected)
                    );
                }
            }
        }
    }
}

#[test]
fn public_region_unknown_target_trap_stop_and_faults_match_each_return() {
    for scenario in 0..5 {
        let mut before = Fixture::new(false, true, 4, false);
        let mut after = Fixture::new(true, true, 4, false);
        for f in [&mut before, &mut after] {
            match scenario {
                0 => f.bus.ram[COMMANDS as usize] = 3,
                1 => f.cpu.set_a(4, LEN as u32 - 2),
                2 => f.cpu.set_a(5, LEN as u32 - 2),
                3 => {
                    f.cpu.set_a(3, LEN as u32);
                    f.bus.ram[COMMANDS as usize] = 1;
                }
                4 => {
                    f.cpu.set_a(4, SOURCE + 1);
                    f.cpu.set_a(5, DEST + 1);
                    f.bus.window_len = (SOURCE + 2) as usize;
                }
                _ => unreachable!(),
            }
        }
        for budget in [0, 6, 1, 1, 7, 17] {
            let result = paired_batch(&mut before, &mut after, budget, &[TRAP, HANDLER]);
            if matches!(result.exit, BatchExit::WatchedPc { .. }) {
                paired_batch(&mut before, &mut after, 3, &[]);
                break;
            }
        }
        if scenario == 0 {
            for f in [&mut before, &mut after] {
                f.cpu.pc = TRAP + 4;
            }
            assert_eq!(
                paired_batch(&mut before, &mut after, 1, &[]).exit,
                BatchExit::Stopped
            );
            assert_eq!(
                paired_batch(&mut before, &mut after, 10, &[]).instructions,
                0
            );
        }
    }
}

#[test]
fn public_region_configuration_and_precise_api_remain_independent() {
    for scenario in 0..4 {
        let mut before = Fixture::new(false, false, 1, false);
        let mut after = Fixture::new(true, false, 1, false);
        paired_batch(&mut before, &mut after, 128, &[]);
        assert!(candidate_entries(after.jit.as_ref().unwrap()) > 0);
        for f in [&mut before, &mut after] {
            match scenario {
                0 => f.cpu.set_cpu_type(CpuType::M68020),
                1 => {
                    f.cpu.set_sr(0x2000);
                    f.cpu.set_irq(7);
                }
                2 => f.cpu.set_sr(0xa000),
                3 => f.bus.tracked = true,
                _ => unreachable!(),
            }
        }
        paired_batch(&mut before, &mut after, 1, &[]);
        paired_batch(&mut before, &mut after, 7, &[HANDLER]);
        // Cycle APIs retain their own exact accounting despite a populated region cache.
        let a = before.in_jit(|cpu, bus| cpu.run_for_cycles(bus, 80));
        let b = after.in_jit(|cpu, bus| cpu.run_for_cycles(bus, 80));
        assert_eq!(b, a, "precise API result and cycles");
        assert_eq!(architecture(&after.cpu), architecture(&before.cpu));
        assert_eq!(after.cpu.cycles_remaining, before.cpu.cycles_remaining);
        assert_eq!(after.bus.ram, before.bus.ram);
        assert_eq!(after.bus.events, before.bus.events);
    }
}

fn interpreted_batch(f: &mut Fixture, budget: u32, watches: &[u32]) -> BatchResult {
    use crate::StepResult;
    if f.cpu.stopped != 0 {
        return BatchResult {
            instructions: 0,
            exit: BatchExit::Stopped,
        };
    }
    let mut retired = 0;
    while retired < budget {
        let exit = match f.cpu.step(&mut f.bus) {
            StepResult::Ok { .. } => None,
            StepResult::Stopped => {
                retired += 1;
                Some(BatchExit::Stopped)
            }
            StepResult::AlineTrap { opcode } => Some(BatchExit::AlineTrap { opcode }),
            StepResult::FlineTrap { opcode } => Some(BatchExit::FlineTrap { opcode }),
            StepResult::TrapInstruction { trap_num } => {
                Some(BatchExit::TrapInstruction { trap_num })
            }
            StepResult::Breakpoint { bp_num } => Some(BatchExit::Breakpoint { bp_num }),
            StepResult::IllegalInstruction { opcode } => {
                Some(BatchExit::IllegalInstruction { opcode })
            }
        };
        if let Some(exit) = exit {
            return BatchResult {
                instructions: retired,
                exit,
            };
        }
        retired += 1;
        if watches.contains(&f.cpu.pc) {
            return BatchResult {
                instructions: retired,
                exit: BatchExit::WatchedPc { pc: f.cpu.pc },
            };
        }
    }
    BatchResult {
        instructions: retired,
        exit: BatchExit::BudgetExhausted,
    }
}

fn step_visible_architecture(cpu: &CpuCore) -> Vec<(&'static str, String)> {
    // The bounded classification run also tested Disabled: the released batch
    // tier leaves exactly these two scratch fields different from step(). Keep
    // both fields in every original-tier-vs-candidate comparison above.
    assert_eq!(
        cpu.t1_flag | cpu.t0_flag,
        0,
        "step projection is valid only with trace off"
    );
    assert_eq!(cpu.sr_save & 0xc000, 0);
    assert!(cpu.pending_fault_cause.is_none() && cpu.fault_resume.is_none());
    assert!(cpu.last_exception_vector.is_none() && cpu.instruction_exception_vector.is_none());
    // check_trace() consumes/clears the one-instruction flow latch; with saved T
    // bits clear it cannot request an exception. Recent-write scratch supplies
    // a write-fault frame only; each possibly faulting write first replaces it.
    architecture(cpu)
        .into_iter()
        .filter(|(field, _)| !matches!(*field, "change_of_flow" | "pending_fault_wdata"))
        .collect()
}

#[test]
fn public_region_also_matches_the_independent_step_oracle() {
    for width in [1, 2, 4] {
        let mut reference = Fixture::new(false, true, width, false);
        let mut separate = Fixture::new(false, true, width, false);
        let mut candidate = Fixture::new(true, true, width, false);
        for (budget, watches) in [
            (0, vec![]),
            (27, vec![]),
            (1, vec![]),
            (41, vec![COPY]),
            (1, vec![]),
            (53, vec![ARM]),
            (1, vec![]),
            (61, vec![]),
        ] {
            let a = interpreted_batch(&mut reference, budget, &watches);
            let original = separate.batch(budget, &watches);
            let b = candidate.batch(budget, &watches);
            compare_boundary(
                &separate,
                &candidate,
                original,
                b,
                "independent-step classification: original tier vs candidate",
            );
            assert_eq!(b, a, "independent step: public retirement and exit");
            assert_eq!(
                step_visible_architecture(&separate.cpu),
                step_visible_architecture(&reference.cpu),
                "independent step: original tier architecture"
            );
            assert_eq!(
                step_visible_architecture(&candidate.cpu),
                step_visible_architecture(&reference.cpu),
                "independent step: candidate architecture"
            );
            assert_eq!(
                candidate.bus.ram, reference.bus.ram,
                "independent step: all guest RAM"
            );
            assert_eq!(
                candidate.bus.events, reference.bus.events,
                "independent step: ordered copy/write effects"
            );
            assert_eq!(reference.bus.pending_copy, None);
            assert_eq!(candidate.bus.pending_copy, None);
        }
        assert!(candidate_entries(candidate.jit.as_ref().unwrap()) > 0);
    }
}

#[test]
fn public_region_condskip_and_indirect_exits_preserve_public_retirement() {
    for skip in [false, true] {
        let mut before = Fixture::new(false, false, 1, false);
        let mut after = Fixture::new(true, false, 1, false);
        for f in [&mut before, &mut after] {
            let enabled = f.enabled;
            let opcode = if skip { 0x6602 } else { 0x6702 };
            f.bus.put(COPY, &[opcode]);
            let jit = f.jit.as_mut().unwrap();
            // Reconstruct the ordinary path from guest bytes. A different region
            // emitter may replace HEAD's cached ops with its widened graph.
            let pcs = [
                HEAD,
                HEAD + 2,
                HEAD + 4,
                HEAD + 6,
                HEAD + 8,
                HEAD + 12,
                COPY,
                COPY + 2,
                COPY + 4,
            ];
            let mut ops: Vec<_> = pcs
                .into_iter()
                .map(|pc| decode_trace_op(&f.cpu, &mut f.bus, pc, CpuType::M68040).unwrap())
                .collect();
            for op in &mut ops {
                if let JitTraceOp::IndirectJmp {
                    expected_target, ..
                } = &mut op.op
                {
                    *expected_target = Some(COPY);
                }
            }
            ops[6] = TraceBuildOp {
                pc: COPY,
                opcode,
                extension: None,
                extension2: None,
                op: JitTraceOp::CondSkip {
                    condition: if skip { 6 } else { 7 },
                    skip_ops: 1,
                    length: 2,
                },
            };
            f.cpu.fm_ptr = f.bus.ram.as_mut_ptr() as usize;
            f.cpu.fm_len = LEN as u32;
            let mut trace = jit
                .compile_decoded_ops(&f.cpu, HEAD, CpuType::M68040, ops, Some(HEAD))
                .unwrap();
            trace.adaptive_branch = false;
            jit.slots[trace_cache_index(HEAD)] = TraceSlot::Compiled(trace);
            configure_candidate(jit, enabled, &[HEAD, ARM, ARM2]);
            f.cpu.fm_ptr = 0;
            f.cpu.fm_len = 0;
            f.bus.ram[COMMANDS as usize..COMMANDS as usize + 7]
                .copy_from_slice(&[0, 0, 1, 0, 0, 2, 3]);
        }
        for budget in [0, 8, 9, 1, 17, 3, 30] {
            paired_batch(&mut before, &mut after, budget, &[]);
        }
        assert!(candidate_entries(after.jit.as_ref().unwrap()) > 0);
    }
}
