// SPDX-License-Identifier: GPL-3.0-or-later

//! Machine-side support for the precise core's wait-loop skip
//! (`src/emulator/wait_skip.rs`): telling whether an instruction stayed in
//! cache lines and plain memory, the fingerprint two loop turns are
//! compared by, and advancing the machine through skipped turns.
//!
//! A turn of a loop is *pure* when every access it makes hits a cache
//! model or reads ROM or fast RAM whose cycle cost depends only on the
//! CPU's own clock carries: no write, no chip-bus or device access, no
//! expansion-bus or Ramsey cycle, no exception or interrupt (both write the
//! stack). Such a turn is a function of the CPU-side state it starts from
//! and of the bytes it reads, nothing else. When it ends in the state it
//! started from, every further turn repeats it exactly -- same costs, same
//! reads -- until an interrupt arrives or something changes the bytes.

use super::*;

/// The CPU-side state at a loop's head: everything a pure turn reads or
/// leaves behind. Equal fingerprints at two heads, with no bytes read
/// changed, mean the turns from there are identical.
pub(crate) struct LoopFingerprint {
    /// The core and the machine's runtime state, as their save-state bytes.
    core: Vec<u8>,
    icache: Option<Box<crate::cache::CpuCache>>,
    dcache: Option<Box<crate::cache::CpuCache>>,
    /// The `CpuBus` latches the next instruction's accesses start from.
    latches: (bool, bool, u8, u8, u8),
    bus: crate::bus::CpuSideBusState,
}

impl LoopFingerprint {
    pub(crate) fn same_as(&self, other: &Self) -> bool {
        self.latches == other.latches
            && self.bus == other.bus
            && self.core == other.core
            && self.icache == other.icache
            && self.dcache == other.dcache
    }
}

/// The cheap part of [`LoopFingerprint`]: registers, status and the
/// sub-colour-clock carries, which a loop's turns cycle through. Equal keys
/// at two heads make it worth comparing full fingerprints there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LoopKey {
    regs: [u32; 16],
    pc: u32,
    sr: u16,
    clock_carry: u32,
    bus: crate::bus::CpuSideBusState,
}

impl LoopKey {
    /// Whether the registers and status register are the same: a wait
    /// loop's turns leave them alone, while a counting or searching loop
    /// changes them every turn.
    pub(crate) fn same_registers(&self, other: &Self) -> bool {
        self.regs == other.regs && self.pc == other.pc && self.sr == other.sr
    }
}

impl CpuBus {
    /// The instruction just made an access the wait-loop skip cannot prove
    /// periodic (see `wait_impure`).
    #[inline]
    pub(super) fn note_wait_impure(&mut self) {
        self.wait_impure = self.wait_impure.wrapping_add(1);
    }

    /// A plain-memory read that missed the caches, kept while a loop is
    /// being proven so later turns can be checked against the same bytes.
    #[inline]
    pub(super) fn note_wait_read(&mut self, addr: u32, size: usize, value: u32) {
        if let Some(reads) = self.wait_reads.as_mut() {
            reads.push((addr, size as u8, value));
        }
    }

    /// What a plain-memory read of `size` bytes at `addr` returns now,
    /// without side effects; `None` outside plain ROM and fast RAM.
    fn plain_peek(&self, addr: u32, size: usize) -> Option<u32> {
        let bytes: &[u8] = match self.classify_plain_memory(addr, size)? {
            PlainMemRegion::OverlayRom(off) | PlainMemRegion::Rom(off) => {
                &self.bus.mem.rom[off..off + size]
            }
            PlainMemRegion::ExtendedRom(off) => &self.bus.mem.extended_rom[off..off + size],
            PlainMemRegion::Wcs(off) => &self.bus.mem.wcs[off..off + size],
            PlainMemRegion::AccelRam(off) => &self.bus.mem.accel_ram[off..off + size],
            PlainMemRegion::MbRam(off) => &self.bus.mem.mb_ram[off..off + size],
            PlainMemRegion::ZorroRam(board, off) => {
                &self.bus.mem.zorro.board_ram(board)[off..off + size]
            }
            PlainMemRegion::ChipRam(_)
            | PlainMemRegion::SlowRam(_)
            | PlainMemRegion::Cartridge(_) => return None,
        };
        Some(bytes.iter().fold(0, |v, &b| v << 8 | u32::from(b)))
    }
}

impl M68kMachine {
    /// The count of accesses no wait-loop proof can rely on, so far.
    pub(crate) fn wait_impure_count(&self) -> u32 {
        self.bus.wait_impure
    }

    /// Whether this machine can skip a proven wait loop's turns at all:
    /// the precise core with no debugger, trace or profiling hook (each of
    /// those observes every instruction), and nothing watching the bus.
    pub(crate) fn wait_skip_supported(&self) -> bool {
        !self.jit_enabled
            && !self.precise_debug_hooks_active()
            && self.bus.bus.cpu_progress_unobserved()
    }

    /// Start, or stop, keeping the plain-memory reads that miss the caches.
    pub(crate) fn record_wait_reads(&mut self, on: bool) {
        match (on, self.bus.wait_reads.as_mut()) {
            (true, Some(reads)) => reads.clear(),
            (true, None) => self.bus.wait_reads = Some(Vec::new()),
            (false, _) => self.bus.wait_reads = None,
        }
    }

    /// How many reads have been kept since recording started.
    pub(crate) fn wait_reads_len(&self) -> usize {
        self.bus.wait_reads.as_ref().map_or(0, Vec::len)
    }

    /// The reads kept since recording started, from the `range` of them.
    pub(crate) fn wait_reads(&self, range: std::ops::Range<usize>) -> Vec<(u32, u8, u32)> {
        self.bus
            .wait_reads
            .as_ref()
            .and_then(|reads| reads.get(range))
            .map(<[_]>::to_vec)
            .unwrap_or_default()
    }

    /// Whether every recorded read would return what it returned then.
    pub(crate) fn wait_reads_unchanged(&self, reads: &[(u32, u8, u32)]) -> bool {
        reads
            .iter()
            .all(|&(addr, size, value)| self.bus.plain_peek(addr, size.into()) == Some(value))
    }

    /// The CPU-side state at this instruction boundary (see
    /// [`LoopFingerprint`]).
    pub(crate) fn wait_fingerprint(&self) -> Result<LoopFingerprint> {
        let mut core = Vec::with_capacity(4096);
        serialize_component(&mut core, &self.cpu, "CPU core")?;
        serialize_component(&mut core, &self.runtime_state(), "machine runtime")?;
        Ok(LoopFingerprint {
            core,
            icache: self.bus.icache.clone(),
            dcache: self.bus.dcache.clone(),
            latches: (
                self.bus.last_fetch_cache_hit,
                self.bus.unmapped_read_continues_word,
                self.bus.sampled_operand_irq_level,
                self.bus.sr_ipl_mask,
                self.bus.sampled_irq_level,
            ),
            bus: self.bus.bus.cpu_side_state(),
        })
    }

    /// Whether an interrupt the CPU would take is asserted at its pins now:
    /// one already raised, which the next instruction's accesses will
    /// sample. The event horizon covers only interrupts still to come.
    pub(crate) fn wait_interrupt_asserted(&self) -> bool {
        let level = self.pending_irq_level();
        level != 0 && (level == 7 || level > ((self.cpu.get_sr() >> 8) & 7) as u8)
    }

    /// See [`LoopKey`].
    pub(crate) fn wait_key(&self) -> LoopKey {
        LoopKey {
            regs: self.cpu.dar,
            pc: self.cpu.pc,
            sr: self.cpu.get_sr(),
            clock_carry: self.cpu_clock_carry,
            bus: self.bus.bus.cpu_side_state(),
        }
    }

    /// Advance the machine through `cck` colour clocks of skipped loop
    /// turns, as one CPU slice: the chipset and the timed devices advance
    /// exactly as the turns' instructions would have advanced them, and
    /// the CPU, which those turns leave as they found it, is untouched.
    /// Returns the colour clocks the slice advanced.
    pub(crate) fn skip_wait_turns(&mut self, cck: u32) -> u32 {
        self.bus.bus.begin_cpu_slice();
        self.bus.bus.slice_preempted = false;
        self.bus.bus.set_cpu_bus_arbitration_enabled(true);
        self.bus.bus.set_cpu_access_phase_sync(true);
        self.bus.bus.advance_cpu_internal_cycles(cck);
        self.bus.flush_timed_devices_dcache_coherent();
        self.bus.bus.set_cpu_bus_arbitration_enabled(false);
        self.bus.bus.take_slice_bus_advance().0
    }
}
