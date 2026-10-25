//! Bounded background compilation. Only immutable IR crosses into the worker.
//! Executable modules never leave that thread; a lease keeps each installed
//! entry alive, including after its originating client has been dropped.

use super::{PrivateCompiledRegion, RegionFn, RegionInput};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Weak};

struct Request {
    id: u64,
    input: RegionInput,
    // The worker must not retain a permanent sender to its own input channel.
    // Transfer this per-request sender to the result's lease instead.
    keepalive: SyncSender<Request>,
}

pub(super) struct CodeLease {
    _keepalive: SyncSender<Request>,
}

pub(super) struct Completion {
    pub(super) id: u64,
    pub(super) input: RegionInput,
    pub(super) code: Result<(RegionFn, usize), &'static str>,
    // Drop input before lease: the worker still owns the large IR allocations
    // when the foreground releases its small snapshot of Arc handles.
    pub(super) lease: Arc<CodeLease>,
}

struct OwnedCode {
    compiled: Option<PrivateCompiledRegion>,
    _input: RegionInput,
    lease: Weak<CodeLease>,
}

impl Drop for OwnedCode {
    fn drop(&mut self) {
        if let Some(compiled) = self.compiled.take() {
            // SAFETY: OwnedCode exists only on the worker. It is removed only
            // after its lease has expired, or when all request senders have
            // disconnected. Every published entry owns such a sender/lease.
            if self.lease.strong_count() == 0 {
                unsafe { compiled.module.free_memory() };
            }
            // An unexpected unwind outside the compiler catch may drop this
            // record while an installed entry is still reachable. JITModule's
            // ordinary Drop deliberately leaks executable memory; prefer that
            // bounded failure leak to unmapping code another thread can run.
        }
    }
}

pub(in super::super) struct Worker {
    sender: SyncSender<Request>,
    results: Receiver<Completion>,
    pending: Option<(u64, u32)>,
    next_id: u64,
    poll_countdown: u8,
    disconnected: bool,
}

impl Worker {
    #[cfg(all(target_arch = "x86_64", any(not(test), not(feature = "trace-profile"))))]
    pub(in super::super) fn new() -> Option<Self> {
        Self::new_with_compiler(super::compile_private_snapshot)
    }

    // Statically dispatched seam for deterministic thread-lifecycle tests.
    // The normal worker still invokes exactly compile_private_snapshot; no
    // test hook or extra branch is added to foreground trace execution.
    pub(super) fn new_with_compiler<F>(compiler: F) -> Option<Self>
    where
        F: FnMut(&RegionInput) -> Option<PrivateCompiledRegion> + Send + 'static,
    {
        let (sender, requests) = mpsc::sync_channel::<Request>(1);
        let (completed, results) = mpsc::sync_channel::<Completion>(1);
        // Dropping JoinHandle detaches: client destruction never joins a job.
        let _thread = std::thread::Builder::new()
            .name("m68k-region-compiler".into())
            .spawn(move || run(requests, completed, compiler))
            .ok()?;
        Some(Self {
            sender,
            results,
            pending: None,
            next_id: 0,
            poll_countdown: 0,
            disconnected: false,
        })
    }

    #[inline]
    pub(in super::super) fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn submit(&mut self, input: RegionInput) -> Result<(), &'static str> {
        if self.disconnected {
            return Err("region compiler disconnected");
        }
        if self.pending.is_some() {
            return Err("region compilation already pending");
        }
        let root = input.heads[0].pc;
        let id = self.next_id;
        let request = Request {
            id,
            input,
            keepalive: self.sender.clone(),
        };
        match self.sender.try_send(request) {
            Ok(()) => {
                self.next_id = self.next_id.wrapping_add(1);
                self.pending = Some((id, root));
                self.poll_countdown = 0;
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err("region compiler queue full"),
            Err(TrySendError::Disconnected(_)) => {
                self.disconnected = true;
                Err("region compiler disconnected")
            }
        }
    }

    pub(super) fn poll(&mut self, pc: u32) -> Option<Completion> {
        let (id, root) = self.pending?;
        if self.poll_countdown != 0 {
            self.poll_countdown -= 1;
            return None;
        }
        // Polling is host policy only: it never changes the guest work budget.
        // Also drain a result when its former root stops executing. Otherwise
        // one stale job could block every later candidate indefinitely.
        self.poll_countdown = if pc == root { 31 } else { 63 };
        match self.results.try_recv() {
            Ok(completion) => {
                self.pending = None;
                if completion.id == id {
                    Some(completion)
                } else {
                    // A protocol failure cannot publish an unrelated entry.
                    self.disconnected = true;
                    None
                }
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                self.disconnected = true;
                None
            }
        }
    }
}

fn run<F>(requests: Receiver<Request>, completed: SyncSender<Completion>, mut compiler: F)
where
    F: FnMut(&RegionInput) -> Option<PrivateCompiledRegion>,
{
    let mut owned: Vec<OwnedCode> = Vec::new();
    while let Ok(request) = requests.recv() {
        owned.retain(|record| record.lease.strong_count() != 0);
        // Existing installed modules survive a compiler panic. The private
        // compiler's unpublished-module guard handles the unfinished module.
        let compiled = catch_unwind(AssertUnwindSafe(|| compiler(&request.input)));
        let (compiled, code) = match compiled {
            Ok(Some(code)) => {
                let entry = (code.entry, code.inlined_calls);
                (Some(code), Ok(entry))
            }
            Ok(None) => (None, Err("background region compilation refused")),
            Err(_) => (None, Err("background region compiler panicked")),
        };
        let lease = Arc::new(CodeLease {
            _keepalive: request.keepalive,
        });
        // Retain IR even for a refused result until foreground observation.
        owned.push(OwnedCode {
            compiled,
            _input: request.input.clone(),
            lease: Arc::downgrade(&lease),
        });
        let result = Completion {
            id: request.id,
            input: request.input,
            code,
            lease,
        };
        // One request outstanding means one completion. Never wait on a host
        // that stopped polling or dropped its CPU while this job was running.
        let _ = completed.try_send(result);
        owned.retain(|record| record.lease.strong_count() != 0);
    }
    // No sender can disappear while an installed CodeLease remains. Therefore
    // disconnect also proves all executable entries are no longer reachable.
    debug_assert!(owned.iter().all(|record| record.lease.strong_count() == 0));
}

#[cfg(all(test, target_arch = "x86_64"))]
#[path = "worker_lifecycle_tests.rs"]
mod lifecycle_tests;
