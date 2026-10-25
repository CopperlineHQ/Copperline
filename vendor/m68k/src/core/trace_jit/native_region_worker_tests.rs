//! Worker-private code-generation checks using the shared public fixture.
//! The guest bytes, complete architecture snapshot, tracked-bus observation and
//! every-return budget/watch assertions are copied from the qualified public
//! oracle. Only compilation/module ownership differs. No worker thread is needed.
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
        RegionMode::Public
    } else {
        RegionMode::Disabled
    };
    let mut jit = TraceJit::new_with_region_mode(mode);
    let module = jit.module.as_mut().expect("native JIT");
    let signature = module.make_signature();
    // Deliberately make retained IDs unrelated to the new private module's ID0.
    for index in 0..32 {
        module
            .declare_function(
                &format!("private_test_padding_{index}"),
                Linkage::Import,
                &signature,
            )
            .unwrap();
    }
    jit
}
fn configure_candidate(jit: &mut TraceJit, enabled: bool, heads: &[u32]) -> Option<JITModule> {
    jit.native_region_enabled = enabled;
    if !enabled {
        return None;
    }
    // The synchronous original path supplies the same descriptor/input. Its
    // native entry is replaced, so every region invocation below uses PRIVATE
    // executable storage, not the foreground module's compiled function.
    jit.compile_native_region(heads)
        .expect("synchronous descriptor admitted");
    let input = jit.native_region.as_ref().unwrap().input.clone();
    assert!(
        input
            .bodies
            .iter()
            .all(|body| body.body_id.as_u32() >= 32 && body.checked_id.as_u32() >= 32)
    );
    let private = compile_private_snapshot(&input).expect("private compilation succeeds");
    assert_eq!(private.inlined_calls, 2 * heads.len());
    assert_eq!(
        private.module.declarations().get_functions().count(),
        1,
        "old declarations must not be copied"
    );
    assert_eq!(private.module.declarations().get_data_objects().count(), 0);
    assert!(
        jit.module
            .as_ref()
            .unwrap()
            .declarations()
            .get_functions()
            .count()
            >= 39
    );
    jit.native_region.as_mut().unwrap().entry = private.entry;
    Some(private.module)
}
fn candidate_entries(jit: &TraceJit) -> u64 {
    jit.native_region_public_entries
}

struct Fixture {
    cpu: CpuCore,
    bus: Bus,
    jit: Option<TraceJit>,
    private_module: Option<JITModule>,
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
        let private_module = configure_candidate(&mut jit, enabled, &[HEAD, arm, ARM2]);
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
            private_module,
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

impl Drop for Fixture {
    fn drop(&mut self) {
        // First remove every path that can call this private entry. Tests only
        // invoke synchronously through their scoped TLS installation.
        drop(self.jit.take());
        if let Some(module) = self.private_module.take() {
            // SAFETY: no entry is running, the only owning JIT was dropped, and
            // these tests never export a function pointer outside this fixture.
            unsafe { module.free_memory() };
        }
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
fn private_module_matches_public_budgets_watches_and_copy_notifications() {
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
fn private_module_revalidates_changed_arm_with_exact_tracked_effects() {
    for (width, location) in [(1, 0), (2, 2), (4, 3)] {
        for protected in [false, true] {
            for first_budget in [6, 7] {
                for watch_kind in [0, 2] {
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
fn private_entry_retires_guest_work_without_a_foreground_call_driver() {
    let mut fixture = Fixture::new(true, false, 1, false);
    let entry = fixture
        .jit
        .as_ref()
        .unwrap()
        .native_region
        .as_ref()
        .unwrap()
        .entry;
    fixture.cpu.fm_ptr = fixture.bus.ram.as_mut_ptr() as usize;
    fixture.cpu.fm_base = 0;
    fixture.cpu.fm_len = LEN as u32;
    let mut exit = RegionExit::default();
    // SAFETY: private module and backing RAM outlive this call; no bus/CPU
    // observer aliases the CPU and raw-memory mode has no tracked callbacks.
    unsafe { entry(&mut fixture.cpu, 31, 0, &mut exit) };
    assert!(exit.retired > 0 && exit.retired <= 31);
    assert_ne!(
        fixture.cpu.a(4),
        SOURCE,
        "guest copy actually advanced its source"
    );
}

fn edited_first_body(input: &RegionInput, edit: impl FnOnce(&mut Function)) -> RegionInput {
    let mut changed = input.clone();
    let old = &input.bodies[0];
    let mut body = InlineBody {
        body_id: old.body_id,
        body: old.body.clone(),
        checked_id: old.checked_id,
        checked: old.checked.clone(),
    };
    edit(&mut body.body);
    changed.bodies[0] = Arc::new(body);
    changed
}

fn expect_private_refusal(input: &RegionInput) {
    if let Some(unexpected) = compile_private_snapshot(input) {
        // No entry from this unexpected result was published or called.
        unsafe { unexpected.module.free_memory() };
        panic!("private compilation accepted incompatible/foreground-dependent IR");
    }
}

#[test]
fn private_module_rejects_abi_mismatch_and_live_foreground_symbol() {
    use cranelift_codegen::cursor::{Cursor, FuncCursor};
    use cranelift_codegen::ir::Signature;
    let fixture = Fixture::new(true, false, 1, false);
    let input = fixture
        .jit
        .as_ref()
        .unwrap()
        .native_region
        .as_ref()
        .unwrap()
        .input
        .clone();
    let wrong_abi = edited_first_body(&input, |body| {
        body.signature.params[0].value_type = types::I8;
    });
    expect_private_refusal(&wrong_abi);
    for address_only in [false, true] {
        let residual = edited_first_body(&input, |body| {
            let call_conv = body.signature.call_conv;
            let signature = body.import_signature(Signature::new(call_conv));
            let name = body.declare_imported_user_function(UserExternalName::new(7, 0x12345));
            let callee = body.import_function(ExtFuncData {
                name: ExternalName::User(name),
                signature,
                colocated: false,
                patchable: false,
            });
            let first = body
                .layout
                .first_inst(body.layout.entry_block().unwrap())
                .unwrap();
            let ptr = body.signature.params[0].value_type;
            let mut cursor = FuncCursor::new(body).at_inst(first);
            if address_only {
                cursor.ins().func_addr(ptr, callee);
            } else {
                cursor.ins().call(callee, &[]);
            }
        });
        expect_private_refusal(&residual);
    }
}

#[test]
fn unused_foreground_import_does_not_require_its_declaration() {
    use cranelift_codegen::ir::Signature;
    let fixture = Fixture::new(true, false, 1, false);
    let input = fixture
        .jit
        .as_ref()
        .unwrap()
        .native_region
        .as_ref()
        .unwrap()
        .input
        .clone();
    let unused = edited_first_body(&input, |body| {
        let call_conv = body.signature.call_conv;
        let signature = body.import_signature(Signature::new(call_conv));
        let name = body.declare_imported_user_function(UserExternalName::new(7, 0x12345));
        body.import_function(ExtFuncData {
            name: ExternalName::User(name),
            signature,
            colocated: false,
            patchable: false,
        });
    });
    let private = compile_private_snapshot(&unused).expect("unused import is harmless");
    assert_eq!(private.module.declarations().get_functions().count(), 1);
    // No pointer from this result was published or called.
    unsafe { private.module.free_memory() };
}
