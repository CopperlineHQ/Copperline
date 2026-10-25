//! Actual worker-thread ownership checks. Channels establish each ordering;
//! timeouts bound failures, rather than estimating progress with sleeps.
use super::super::{Head, RegionExit};
use super::*;
use crate::CpuType;
use crate::core::cpu::CpuCore;
use cranelift_codegen::ir::{AbiParam, Function, InstBuilder, MemFlags, UserFuncName, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use std::mem::{offset_of, transmute};
use std::thread::{self, ThreadId};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

struct Exited(mpsc::Sender<ThreadId>);

impl Drop for Exited {
    fn drop(&mut self) {
        let _ = self.0.send(thread::current().id());
    }
}

// A failed assertion must release the held compiler as the test unwinds.
struct ReleaseOnDrop(SyncSender<()>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

fn input(root: u32) -> RegionInput {
    RegionInput {
        heads: vec![Head {
            pc: root,
            cpu_type: CpuType::M68040,
            ops: 1,
            max_cycles: 1,
            native_loop: false,
            needs_window: false,
            tracked_writes: false,
            checked: 0,
            body: 0,
        }],
        // Protocol tests intentionally do not supply real trace IR. The
        // injected compiler emits a canary with the exact RegionFn ABI.
        bodies: Vec::new(),
        public: true,
    }
}

fn compile_canary(input: &RegionInput) -> Option<PrivateCompiledRegion> {
    let mut module = JITModule::new(JITBuilder::new(default_libcall_names()).unwrap());
    let ptr = module.target_config().pointer_type();
    let mut signature = module.make_signature();
    for ty in [ptr, types::I32, types::I32, ptr] {
        signature.params.push(AbiParam::new(ty));
    }
    let id = module
        .declare_function("worker_lifetime_canary", Linkage::Local, &signature)
        .unwrap();
    let mut context = module.make_context();
    context.func = Function::with_name_signature(UserFuncName::user(0, id.as_u32()), signature);
    let mut frontend = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
        let block = builder.create_block();
        builder.switch_to_block(block);
        builder.append_block_params_for_function_params(block);
        let output = builder.block_params(block)[3];
        let value = builder
            .ins()
            .iconst(types::I32, i64::from(input.heads[0].pc));
        builder.ins().store(
            MemFlags::new(),
            value,
            output,
            offset_of!(RegionExit, retired) as i32,
        );
        builder.ins().return_(&[]);
        builder.seal_all_blocks();
        builder.finalize();
    }
    module.define_function(id, &mut context).unwrap();
    module.finalize_definitions().unwrap();
    // SAFETY: the generated signature is precisely RegionFn. The module
    // remains worker-owned; only its pointer and a lifetime lease escape.
    let entry = unsafe { transmute::<*const u8, RegionFn>(module.get_finalized_function(id)) };
    Some(PrivateCompiledRegion {
        module,
        entry,
        inlined_calls: 0,
    })
}

fn call_canary(entry: RegionFn, _lease: &CodeLease, expected: u32) {
    let mut output = RegionExit::default();
    let mut cpu = CpuCore::new();
    // SAFETY: a live lease keeps this finalized entry mapped. The generated
    // function writes only output.retired and never dereferences the CPU.
    unsafe { entry(&mut cpu, 0, 0, &mut output) };
    assert_eq!(output.retired, expected);
}

fn completion(worker: &mut Worker) -> Completion {
    // Ownership/lifetime tests wait on the actual worker's result channel.
    // Separate tests below exercise foreground poll/throttle/protocol logic;
    // a blocking receive here is test synchronization, not foreground policy.
    let result = worker.results.recv_timeout(TIMEOUT).unwrap();
    assert_eq!(worker.pending.take().unwrap().0, result.id);
    result
}

#[test]
fn native_region_worker_held_compile_does_not_block_submit_poll_or_client_drop() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let release = ReleaseOnDrop(release_tx);
    let (exited_tx, exited_rx) = mpsc::channel();
    let exited = Exited(exited_tx);
    let mut worker = Worker::new_with_compiler(move |_| {
        let _keep_until_worker_exit = &exited;
        entered_tx.send(thread::current().id()).unwrap();
        release_rx.recv_timeout(TIMEOUT).unwrap();
        None
    })
    .unwrap();
    worker.submit(input(0x2000)).unwrap();
    let compiler_thread = entered_rx.recv_timeout(TIMEOUT).unwrap();
    let (returned_tx, returned_rx) = mpsc::channel();
    let frontend = thread::spawn(move || {
        assert_eq!(
            worker.submit(input(0x3000)),
            Err("region compilation already pending")
        );
        assert!(worker.poll(0x2000).is_none());
        assert!(worker.pending());
        assert!(worker.poll(0x7000).is_none());
        drop(worker);
        returned_tx.send(()).unwrap();
    });
    // The compiler is still held: neither polling nor destruction may join it.
    returned_rx.recv_timeout(TIMEOUT).unwrap();
    assert!(matches!(exited_rx.try_recv(), Err(TryRecvError::Empty)));
    release.0.try_send(()).unwrap();
    assert_eq!(exited_rx.recv_timeout(TIMEOUT).unwrap(), compiler_thread);
    frontend.join().unwrap();
}

#[test]
fn native_region_worker_published_code_outlives_client_until_last_lease() {
    let (exited_tx, exited_rx) = mpsc::channel();
    let exited = Exited(exited_tx);
    let mut worker = Worker::new_with_compiler(move |input| {
        let _keep_until_worker_exit = &exited;
        compile_canary(input)
    })
    .unwrap();
    worker.submit(input(0x123456)).unwrap();
    let result = completion(&mut worker);
    let entry = result.code.unwrap().0;
    let lease = result.lease.clone();
    let last_lease = lease.clone();
    drop(result);
    drop(worker);
    assert!(matches!(exited_rx.try_recv(), Err(TryRecvError::Empty)));
    call_canary(entry, &lease, 0x123456);
    drop(lease);
    call_canary(entry, &last_lease, 0x123456);
    drop(last_lease);
    assert_ne!(
        exited_rx.recv_timeout(TIMEOUT).unwrap(),
        thread::current().id()
    );
}

#[test]
fn native_region_worker_compiler_panic_keeps_prior_code_and_accepts_next_job() {
    let (exited_tx, exited_rx) = mpsc::channel();
    let exited = Exited(exited_tx);
    let mut worker = Worker::new_with_compiler(move |input| {
        let _keep_until_worker_exit = &exited;
        assert_ne!(input.heads[0].pc, 0xdead, "injected compiler panic");
        compile_canary(input)
    })
    .unwrap();
    worker.submit(input(0x1111)).unwrap();
    let first = completion(&mut worker);
    let entry = first.code.unwrap().0;
    worker.submit(input(0xdead)).unwrap();
    let failed = completion(&mut worker);
    assert_eq!(failed.code, Err("background region compiler panicked"));
    call_canary(entry, &first.lease, 0x1111);
    worker.submit(input(0x2222)).unwrap();
    let next = completion(&mut worker);
    call_canary(next.code.unwrap().0, &next.lease, 0x2222);
    drop(worker);
    drop(failed);
    drop(next);
    call_canary(entry, &first.lease, 0x1111);
    drop(first);
    exited_rx.recv_timeout(TIMEOUT).unwrap();
}

fn queued_worker(id: u64, root: u32) -> (Worker, SyncSender<Completion>, Receiver<Request>) {
    let (sender, requests) = mpsc::sync_channel(1);
    let (completed, results) = mpsc::sync_channel(1);
    (
        Worker {
            sender,
            results,
            pending: Some((id, root)),
            next_id: id + 1,
            poll_countdown: 0,
            disconnected: false,
        },
        completed,
        requests,
    )
}

fn refused(worker: &Worker, id: u64, root: u32) -> Completion {
    Completion {
        id,
        input: input(root),
        code: Err("test refusal"),
        lease: Arc::new(CodeLease {
            _keepalive: worker.sender.clone(),
        }),
    }
}

#[test]
fn native_region_worker_nonroot_poll_eventually_drains_ready_completion() {
    let (mut worker, completed, _requests) = queued_worker(7, 0x2000);
    assert!(worker.poll(0x7000).is_none());
    completed.try_send(refused(&worker, 7, 0x2000)).unwrap();
    for _ in 0..63 {
        assert!(worker.poll(0x7000).is_none());
        assert!(worker.pending());
    }
    let result = worker.poll(0x7000).unwrap();
    assert_eq!(result.id, 7);
    assert_eq!(result.input.heads[0].pc, 0x2000);
    assert!(!worker.pending());
    assert!(!worker.disconnected);
}

#[test]
fn native_region_worker_wrong_completion_id_never_publishes_or_accepts_more_jobs() {
    let (mut worker, completed, _requests) = queued_worker(7, 0x2000);
    let result = refused(&worker, 6, 0x3000);
    let lease = Arc::downgrade(&result.lease);
    completed.try_send(result).unwrap();
    assert!(worker.poll(0x2000).is_none());
    assert!(
        lease.upgrade().is_none(),
        "wrong result must release its code lease"
    );
    assert!(!worker.pending());
    assert!(worker.disconnected);
    assert_eq!(
        worker.submit(input(0x4000)),
        Err("region compiler disconnected")
    );
}

#[test]
fn native_region_worker_submission_captures_metadata_and_does_not_borrow_caller() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let release = ReleaseOnDrop(release_tx);
    let mut worker = Worker::new_with_compiler(move |snapshot| {
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(TIMEOUT).unwrap();
        compile_canary(snapshot)
    })
    .unwrap();
    let mut caller_input = input(0x1111);
    worker.submit(caller_input.clone()).unwrap();
    entered_rx.recv_timeout(TIMEOUT).unwrap();
    caller_input.heads[0].pc = 0x2222;
    drop(caller_input);
    release.0.try_send(()).unwrap();
    let result = completion(&mut worker);
    assert_eq!(result.input.heads[0].pc, 0x1111);
    call_canary(result.code.unwrap().0, &result.lease, 0x1111);
}

#[test]
fn native_region_worker_live_lease_survives_unexpected_owner_unwind() {
    let (sender, _requests) = mpsc::sync_channel(1);
    let (published, received) = mpsc::sync_channel(1);
    let owner = thread::spawn(move || {
        let snapshot = input(0x3333);
        let compiled = compile_canary(&snapshot).unwrap();
        let entry = compiled.entry;
        let lease = Arc::new(CodeLease { _keepalive: sender });
        let _owned = OwnedCode {
            compiled: Some(compiled),
            _input: snapshot.clone(),
            lease: Arc::downgrade(&lease),
        };
        published
            .send(Completion {
                id: 0,
                input: snapshot,
                code: Ok((entry, 0)),
                lease,
            })
            .unwrap();
        // Simulate a worker failure outside the compiler catch. OwnedCode must
        // deliberately leave this one executable allocation mapped, because
        // the published entry is still reachable through its live lease.
        panic!("injected owner unwind after publication");
    });
    let result = received.recv_timeout(TIMEOUT).unwrap();
    assert!(owner.join().is_err());
    call_canary(result.code.unwrap().0, &result.lease, 0x3333);
}
