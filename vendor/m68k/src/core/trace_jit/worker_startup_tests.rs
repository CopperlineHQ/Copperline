//! Production startup of the native-region worker, observed from real processes.
//!
//! A unit test cannot answer the question these cover. Inside a test binary
//! `TraceJit::new` returns early with one deterministic configuration and no
//! worker, so a test that constructs a `TraceJit` never performs the
//! environment read nor the worker start that a production process performs.
//! Re-executing the test binary does not help by itself: the child is another
//! test binary and takes the same early return.
//!
//! So each case re-executes this binary with `M68K_STARTUP_PROBE` set, which is
//! the one thing that early return consults. From that point the child runs
//! `TraceJit::new_production`, which is the constructor the program itself
//! runs: the same environment read, the same mode selection, the same worker
//! start, and then the same recorder, promotion, background publication and
//! region entry, all driven through the public `run_batch`. The child reports
//! what it observed and the parent judges it.
//!
//! Nothing here treats a live thread or an enabled flag as evidence. A region
//! is only reported as having run when the region entry counter moved, and a
//! publication is only reported when the status the worker's own poll path
//! sets has been seen.
//!
//! Each child runs two phases. The warmup is allowed to differ — an enabled
//! child stops as soon as it has published and entered a region, a disabled
//! child has nothing to wait for and runs its whole budget — and nothing
//! observed during it is compared. The measurement that follows is identical in
//! every child: the same guest reset, the same batch count, the same
//! instruction budget, no early exit. Only that phase's retired count, register
//! digest and guest memory are compared across cases, so equivalence is a claim
//! about the same work, not about whatever each child happened to do.
#![cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    not(target_family = "wasm"),
    not(feature = "trace-profile")
))]

use super::*;
use crate::core::memory::LinearMemoryBus;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Names the child test so the parent can select it and the child can tell it
/// is the child. Kept in one place so a rename cannot silently stop spawning.
const CHILD: &str = "core::trace_jit::worker_startup_tests::startup_probe_child";
const PROBE: &str = "M68K_STARTUP_PROBE";
const REGIONS: &str = "M68K_NATIVE_REGIONS";
const WORKLOAD: &str = "M68K_STARTUP_WORKLOAD";
/// Generous next to a workload that settles in well under a second, and short
/// enough that a hang fails the suite rather than stalling CI.
const CHILD_TIMEOUT: Duration = Duration::from_secs(60);

const HEAD: u32 = 0x2000;
const COMMANDS: u32 = 0x5000;
const SOURCE: u32 = 0x3000;
const VALUE: u32 = 0x6000;
const DEST: u32 = 0x4000;
const PLAIN: u32 = 0x2200;
const MEMORY: usize = 0x8000;
const DATA_LEN: u32 = 0x1000;

/// One guest batch, small enough that the advancing pointers stay well inside
/// their own buffers between resets.
const BATCH: u32 = 512;
/// Warmup for off/non-qualifying cases. Enabled qualifying cases instead
/// continue until actual entry or the separate wall-clock deadline.
const WARMUP_BATCHES: u32 = 512;
/// The measured phase, run identically by every child.
const MEASURED_BATCHES: u32 = 64;

/// What the child saw. Every field is observed after real execution, never
/// inferred from configuration.
#[derive(Debug, Default, PartialEq, Eq)]
struct Observed {
    /// A worker thread was constructed by production startup.
    worker: bool,
    /// The status only the worker's publication path sets was seen.
    published: bool,
    /// Region code actually executed, and how often.
    entries: u64,
    /// Of those, entries that ran the public-contract region.
    public_entries: u64,
    /// Region IR is retained at the end of the run.
    retained: bool,
    /// Ordinary traces actually compiled, even with regions disabled.
    compiled: usize,
    /// Guest instructions retired during the measured phase.
    retired: u64,
    /// A digest of the guest registers after every measured batch.
    digest: u64,
    /// A digest of the guest memory the workload writes, taken after every
    /// measured batch. Registers alone would not notice a misplaced store.
    memory: u64,
}

impl Observed {
    fn line(&self) -> String {
        format!(
            "OBSERVED worker={} published={} entries={} public_entries={} retained={} compiled={} retired={} digest={} memory={}",
            self.worker,
            self.published,
            self.entries,
            self.public_entries,
            self.retained,
            self.compiled,
            self.retired,
            self.digest,
            self.memory
        )
    }

    fn parse(output: &str) -> Self {
        let line = output
            .lines()
            .find(|line| line.starts_with("OBSERVED "))
            .unwrap_or_else(|| panic!("the child printed no observation:\n{output}"));
        let mut observed = Self::default();
        let mut seen = std::collections::BTreeSet::new();
        for field in line.trim_start_matches("OBSERVED ").split_whitespace() {
            let (name, value) = field.split_once('=').expect("name=value");
            assert!(seen.insert(name), "duplicate field {name}");
            match name {
                "worker" => observed.worker = value.parse().expect("boolean field"),
                "published" => observed.published = value.parse().expect("boolean field"),
                "entries" => observed.entries = value.parse().unwrap(),
                "public_entries" => observed.public_entries = value.parse().unwrap(),
                "retained" => observed.retained = value.parse().expect("boolean field"),
                "compiled" => observed.compiled = value.parse().unwrap(),
                "retired" => observed.retired = value.parse().unwrap(),
                "digest" => observed.digest = value.parse().unwrap(),
                "memory" => observed.memory = value.parse().unwrap(),
                other => panic!("unexpected field {other}"),
            }
        }
        assert_eq!(seen.len(), 9, "missing observation field: {output}");
        observed
    }
}

/// Drain both streams concurrently so diagnostics cannot fill a pipe and
/// stall the child. Join the readers after the child has exited or been killed.
fn drain<R: Read + Send + 'static>(mut pipe: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = String::new();
        pipe.read_to_string(&mut text).expect("read child output");
        text
    })
}

/// Run one case in a fresh process, so its thread-local compiler, its worker
/// and its environment are its own.
///
/// The environment is set on the child only. The parent's own environment is
/// never touched, so cases cannot race each other and cannot disturb the rest
/// of the suite.
fn observe(regions: Option<&str>, workload: &str) -> Observed {
    observe_os(regions.map(std::ffi::OsStr::new), workload)
}

/// Preserve malformed OS strings so the real environment parser is exercised.
fn observe_os(regions: Option<&std::ffi::OsStr>, workload: &str) -> Observed {
    let exe = std::env::current_exe().expect("the running test binary");
    let mut command = Command::new(exe);
    command
        .args([
            CHILD,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE, "1")
        .env(WORKLOAD, workload)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match regions {
        Some(value) => command.env(REGIONS, value),
        // Absent means absent: inherited state would make the case meaningless.
        None => command.env_remove(REGIONS),
    };
    let mut child = command.spawn().expect("spawn the startup probe");

    let out = drain(child.stdout.take().expect("piped stdout"));
    let err = drain(child.stderr.take().expect("piped stderr"));
    let deadline = Instant::now() + CHILD_TIMEOUT;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("poll the startup probe") {
            break (status, false);
        }
        if Instant::now() >= deadline {
            // It may have exited between try_wait and kill. Reap it either way.
            let _ = child.kill();
            break (child.wait().expect("reap the startup probe"), true);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // Only after the child is gone, so the readers are guaranteed to return.
    let output = out.join().expect("read the probe's stdout");
    let errors = err.join().expect("read the probe's stderr");
    let report = format!(
        "regions={regions:?} workload={workload}\n\
         --- stdout ---\n{output}\n--- stderr ---\n{errors}"
    );
    assert!(
        !timed_out,
        "the startup probe did not finish within {CHILD_TIMEOUT:?}: {report}"
    );
    assert!(
        status.success(),
        "the startup probe failed ({status}) for {report}"
    );
    eprintln!("{report}");
    Observed::parse(&output)
}

/// A table dispatch whose handlers branch back to it: the shape a region is
/// allowed to combine. One handler copies a byte from `(a4)+` to `(a5)+`, so
/// the workload leaves guest memory behind to compare. The recorder compiles
/// this during the run; nothing here installs a trace, so the child exercises
/// discovery and promotion too.
///
/// Written once per process. Rewriting code between batches would be a guest
/// store into a compiled page and would throw away the very caches the warmup
/// exists to fill.
fn install_dispatch_code(bus: &mut LinearMemoryBus) {
    for (index, &word) in [
        0x1a1au16, 0x7000, 0x1005, 0xd040, 0x303b, 0x0006, 0x4efb, 0x0002, 0x0008, 0x000e, 0x0014,
        0x00f0, 0x1adc, 0x5281, 0x60e2, 0x3413, 0x5283, 0x60dc, 0x5286, 0x5287, 0x60d6,
    ]
    .iter()
    .enumerate()
    {
        bus.write_word(HEAD + 2 * index as u32, word);
    }
}

/// The data the dispatch reads and writes, restored to a known state. Only
/// data: none of these addresses holds guest code.
fn install_dispatch_data(bus: &mut LinearMemoryBus) {
    for i in 0..DATA_LEN {
        bus.write_byte(
            COMMANDS + i,
            [0, 0, 0, 0, 0, 0, 0, 0, 1, 2][(i % 10) as usize],
        );
        bus.write_byte(SOURCE + i, (i as u8).wrapping_mul(17).wrapping_add(5));
        bus.write_byte(DEST + i, 0);
    }
    for (index, &word) in [0x8123u16, 0x4567, 0x89ab].iter().enumerate() {
        bus.write_word(VALUE + 2 * index as u32, word);
    }
}

/// A counted register loop with no dispatch at all, so no group of heads can
/// ever qualify. An enabled run over this must not report a region.
fn install_plain_code(bus: &mut LinearMemoryBus) {
    for (index, &word) in [0x5281u16, 0x5282, 0x5283, 0x51c8, 0xfff8]
        .iter()
        .enumerate()
    {
        bus.write_word(PLAIN + 2 * index as u32, word);
    }
}

/// Put the guest back exactly where each batch starts.
///
/// Every pointer the workload advances is restored, so a batch can never walk
/// out of its buffer however many batches precede it, and so batch *n* does the
/// same work as batch 0 whatever the JIT did in between.
fn reset_registers(cpu: &mut CpuCore, dispatching: bool) {
    for index in 0..8 {
        cpu.set_d(index, 0);
    }
    cpu.set_sr(0x2700);
    if dispatching {
        cpu.pc = HEAD;
        cpu.set_a(2, COMMANDS);
        cpu.set_a(3, VALUE);
        cpu.set_a(4, SOURCE);
        cpu.set_a(5, DEST);
    } else {
        cpu.pc = PLAIN;
        cpu.set_d(0, 0x3fff);
    }
    cpu.set_a(7, 0x7000);
}

fn fold(digest: u64, value: u64) -> u64 {
    (digest ^ value).wrapping_mul(0x100_0000_01b3)
}

fn fold_registers(digest: u64, cpu: &CpuCore) -> u64 {
    let mut digest = digest;
    for value in cpu
        .dar
        .iter()
        .copied()
        .chain([cpu.pc, cpu.ppc, cpu.ir, u32::from(cpu.get_ccr())])
    {
        digest = fold(digest, u64::from(value));
    }
    digest
}

/// The bytes the workload can reach: everything the copy handler writes, plus
/// the buffers it reads, so a store landing in the wrong buffer is caught too.
fn fold_memory(digest: u64, bus: &mut LinearMemoryBus) -> u64 {
    let mut digest = digest;
    for base in [DEST, COMMANDS, SOURCE] {
        for offset in 0..DATA_LEN {
            digest = fold(digest, u64::from(bus.read_byte(base + offset)));
        }
    }
    digest
}

/// Latch what the JIT has done so far, and report whether the enabled case has
/// everything it is waiting for. Only counters and the worker's own status are
/// consulted; a live thread on its own proves nothing.
fn sample(observed: &mut Observed) -> bool {
    with_trace_jit(|jit| {
        observed.worker |= jit.native_region_worker.is_some();
        observed.published |= jit.native_region_status == "background region installed";
        observed.entries = jit.native_region_entries;
        observed.public_entries = jit.native_region_public_entries;
        observed.compiled = jit
            .slots
            .iter()
            .filter(|slot| matches!(slot, TraceSlot::Compiled(_)))
            .count();
        // An installed region and retained first-tier IR are different things.
        observed.retained |= jit
            .slots
            .iter()
            .any(|slot| matches!(slot, TraceSlot::Compiled(trace) if trace.region_ir.is_some()));
        observed.published && observed.entries > 0
    })
}

/// The child half. Ignored so an ordinary run never executes it; the parent
/// selects it explicitly.
#[test]
#[ignore = "spawned by the startup probes with a prepared environment"]
fn startup_probe_child() {
    assert!(
        std::env::var_os(PROBE).is_some(),
        "the child must run with production startup in force"
    );
    let dispatching = std::env::var(WORKLOAD).as_deref() != Ok("plain");

    let mut bus = LinearMemoryBus::new(MEMORY);
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);
    if dispatching {
        install_dispatch_code(&mut bus);
        install_dispatch_data(&mut bus);
    } else {
        install_plain_code(&mut bus);
    }

    if dispatching {
        // First record the read arm, then change the guest data to favor the
        // copy arm. The ordinary adaptive recorder must re-record once before
        // a root becomes eligible for combining. An even 0/1/2 mix never
        // crosses its mismatch threshold and therefore is not a qualifying
        // startup workload. No private JIT state is forced here.
        for i in 0..DATA_LEN {
            bus.write_byte(COMMANDS + i, 1);
        }
        for _ in 0..64 {
            reset_registers(&mut cpu, true);
            cpu.run_batch(&mut bus, BATCH, &[]);
        }
        install_dispatch_data(&mut bus);
    }
    let mut observed = Observed::default();

    // Warmup. Long enough for the recorder to compile the heads, for promotion
    // to submit, for the worker to publish and for the published region to run.
    // An enabled child leaves as soon as all of that has happened; a disabled
    // child runs the fixed warmup. Only JIT observations survive this loop.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut warmup_batches = 0;
    loop {
        reset_registers(&mut cpu, dispatching);
        cpu.run_batch(&mut bus, BATCH, &[]);
        warmup_batches += 1;
        if sample(&mut observed) {
            break;
        }
        if Instant::now() >= deadline
            || (warmup_batches >= WARMUP_BATCHES && (!dispatching || !observed.worker))
        {
            break;
        }
        // Give the actual worker an opportunity to progress without assuming
        // it completes within a particular number of fast guest batches.
        std::thread::yield_now();
    }

    // Measurement. Identical in every child: same starting guest state, same
    // batch count, same budget, no early exit.
    if dispatching {
        install_dispatch_data(&mut bus);
    }
    observed.retired = 0;
    observed.digest = 0xcbf2_9ce4_8422_2325;
    observed.memory = 0xcbf2_9ce4_8422_2325;
    for _ in 0..MEASURED_BATCHES {
        reset_registers(&mut cpu, dispatching);
        observed.retired += u64::from(cpu.run_batch(&mut bus, BATCH, &[]).instructions);
        observed.digest = fold_registers(observed.digest, &cpu);
        if dispatching {
            observed.memory = fold_memory(observed.memory, &mut bus);
        }
    }
    sample(&mut observed);
    if observed.worker && observed.entries == 0 && dispatching {
        with_trace_jit(|jit| {
            eprintln!("unpromoted workload: {}", jit.native_region_status);
            for slot in &jit.slots {
                if let TraceSlot::Compiled(trace) = slot {
                    eprintln!(
                        "head={:x} ops={} loop={} adaptive={} seeded={} ir={}",
                        trace.pc,
                        trace.ops.len(),
                        trace.native_loop,
                        trace.adaptive_branch,
                        trace.seeded_exit,
                        trace.region_ir.is_some()
                    );
                }
            }
        });
    }
    assert!(
        observed.compiled > 0,
        "ordinary native traces must compile: {observed:?}"
    );
    assert_eq!(observed.retired, u64::from(BATCH * MEASURED_BATCHES));
    println!("\n{}", observed.line());
}

/// The native x86-64 default must publish and execute, not merely start a worker.
#[test]
fn absent_variable_publishes_from_the_worker_and_runs_the_region() {
    let observed = observe(None, "dispatch");
    assert!(observed.worker, "default built a worker: {observed:?}");
    assert!(observed.published, "default published code: {observed:?}");
    assert!(
        observed.public_entries > 0,
        "default ran code: {observed:?}"
    );
    assert!(observed.retained, "region IR retained: {observed:?}");

    let off = observe(Some("off"), "dispatch");
    assert_eq!(observed.retired, off.retired, "same guest work as off");
    assert_eq!(observed.digest, off.digest, "same registers as off");
    assert_eq!(observed.memory, off.memory, "same guest memory as off");
}

/// The escape hatch removes worker/IR overhead while preserving guest results.
#[test]
fn explicit_off_starts_no_worker_and_retains_no_region() {
    let off = observe(Some("off"), "dispatch");
    assert!(!off.worker, "no worker: {off:?}");
    assert!(!off.published, "nothing published: {off:?}");
    assert!(!off.retained, "no region IR retained: {off:?}");
    assert_eq!(off.entries, 0, "no region ran: {off:?}");

    let absent = observe(None, "dispatch");
    assert_eq!(off.retired, absent.retired, "same measured work as absent");
    assert_eq!(off.digest, absent.digest, "same registers as absent");
    assert_eq!(off.memory, absent.memory, "same guest memory as absent");
}

/// Explicitly enabled must start a worker, complete a publication and actually
/// execute the published region, while the measured phase produces the same
/// guest results a process without regions produces.
#[test]
fn explicit_public_publishes_from_the_worker_and_runs_the_region() {
    let enabled = observe(Some("public"), "dispatch");
    assert!(
        enabled.worker,
        "production startup built a worker: {enabled:?}"
    );
    assert!(
        enabled.published,
        "the worker's publication path completed: {enabled:?}"
    );
    assert!(
        enabled.public_entries > 0,
        "the published region actually executed: {enabled:?}"
    );
    assert!(enabled.retained, "region IR retained: {enabled:?}");

    // Same measured guest work, or the region is not doing the guest's job.
    let off = observe(Some("off"), "dispatch");
    assert_eq!(enabled.retired, off.retired, "same measured work as off");
    assert_eq!(enabled.digest, off.digest, "same registers as off");
    assert_eq!(enabled.memory, off.memory, "same guest memory as off");
    assert!(
        enabled.memory != 0xcbf2_9ce4_8422_2325,
        "the memory digest actually covered something: {enabled:?}"
    );
}

/// A workload with nothing to combine must not let the enabled case pass on
/// fallback execution alone: the worker starts, and no region is ever published
/// or entered.
#[test]
fn an_enabled_process_reports_no_region_for_a_workload_that_cannot_qualify() {
    for value in [None, Some("public")] {
        let plain = observe(value, "plain");
        assert!(
            plain.worker,
            "production startup still built a worker: {plain:?}"
        );
        assert!(!plain.published, "nothing to publish: {plain:?}");
        assert!(!plain.retained, "no region IR retained: {plain:?}");
        assert_eq!(plain.entries, 0, "no region ran: {plain:?}");
        assert!(plain.retired > 0, "the guest ran at all: {plain:?}");
    }

    // And the same binary does publish for a workload that qualifies, so the
    // absence above is a property of the workload, not a broken probe.
    let dispatching = observe(Some("public"), "dispatch");
    assert!(
        dispatching.published,
        "the probe can publish: {dispatching:?}"
    );
}

/// Invalid Unicode must not be mistaken for an absent, default-enabled value.
#[test]
#[cfg(any(unix, windows))]
fn malformed_environment_value_keeps_the_worker_disabled() {
    #[cfg(unix)]
    let value = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![0xff])
    };
    #[cfg(windows)]
    let value = {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&[0xd800])
    };
    let observed = observe_os(Some(&value), "dispatch");
    assert!(!observed.worker, "no worker: {observed:?}");
    assert!(!observed.published, "nothing published: {observed:?}");
    assert!(!observed.retained, "no region IR retained: {observed:?}");
    assert_eq!(observed.entries, 0, "no region ran: {observed:?}");

    let off = observe(Some("off"), "dispatch");
    assert_eq!(observed.retired, off.retired, "same guest work as off");
    assert_eq!(observed.digest, off.digest, "same registers as off");
    assert_eq!(observed.memory, off.memory, "same guest memory as off");
}
