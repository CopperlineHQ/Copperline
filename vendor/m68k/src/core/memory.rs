//! Memory access trait.

/// Kind of bus-level fault during a memory access (distinct from 68000 address error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusFaultKind {
    /// Generic bus error (unmapped address, device error, etc).
    BusError,
}

/// A bus-level fault that occurred during a memory access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusFault {
    /// Classification of the failed bus transaction.
    pub kind: BusFaultKind,
    /// Bus address at which the transaction failed.
    pub address: u32,
}

/// A contiguous guest-RAM window the CPU may access directly ("fastmem").
///
/// Returned by [`AddressBus::fast_mem`]. The window lets the batch
/// execution path ([`CpuCore::run_batch`](crate::CpuCore::run_batch))
/// fetch opcodes and execute memory-operand instructions without a bus
/// call per access.
///
/// # Contract
///
/// For every guest address `a` in `[base, base + len)`, and for the whole
/// duration of the `run_batch` call that captured the window:
///
/// - `ptr[a - base]` holds the same byte the bus would return from
///   `read_byte(a)`, stored big-endian for multi-byte values (i.e. the
///   window is the bus's actual backing RAM, not a copy);
/// - reads and writes through `ptr` have **no side effects** — no MMIO,
///   no watchpoints, no dirty tracking, no mirroring — and are fully
///   interchangeable with the `read_*`/`write_*` methods;
/// - the pointer stays valid and the backing storage is not moved or
///   resized, even across interleaved `AddressBus` method calls.
///
/// Buses with any interception (tracers, watchpoints, MMIO in range)
/// must return `None` from `fast_mem` while that interception is active.
/// `len` must be at least 4 bytes; smaller windows are ignored.
///
/// Guest addresses here are physical. With the PMMU enabled the batch path
/// translates each access through the transparent-translation registers
/// and the ATC (no table search) before using the window; an access they
/// cannot map into it goes through the bus as usual.
#[derive(Debug, Clone, Copy)]
pub struct FastMem {
    /// Host pointer to the byte backing guest address `base`.
    pub ptr: *mut u8,
    /// First guest address covered by the window.
    pub base: u32,
    /// Window length in bytes.
    pub len: u32,
}

/// Maximum number of additional direct-memory regions captured per batch.
pub const MAX_FAST_MEM_REGIONS: usize = 8;

/// Additional side-effect-free memory, including read-only ROM.
///
/// The stability, backing-storage and interception requirements of [`FastMem`]
/// apply. `writable = false` permits fetches and reads only: stores and entire
/// read-modify-write instructions fall back to the bus. Regions must not wrap
/// the 32-bit address space or overlap one another/the primary window. Invalid
/// or overlapping entries are ignored. Mapping changes must end the batch
/// before another instruction executes; backing allocations must stay valid
/// until the batch returns. Guest addresses are physical, including with PMMU.
#[derive(Debug, Clone, Copy)]
pub struct FastMemRegion {
    /// Stable host pointer backing the first byte.
    pub ptr: *mut u8,
    /// First physical guest address.
    pub base: u32,
    /// Number of contiguous bytes.
    pub len: u32,
    /// Whether direct stores are side-effect-free and permitted.
    pub writable: bool,
}

/// Read-direct RAM whose writes must still pass through the bus.
///
/// This is an opt-in native-JIT capability for 68020 and later CPUs, not a
/// general MMIO mapping. Earlier CPUs retain interpreter bus sequencing. Reads
/// within the window must have no side effects and must equal the bus methods.
/// The allocation and address mapping must remain stable throughout `run_batch`,
/// including across writes and copy notifications. Writes within the window must
/// be infallible through `write_byte/word/long`; they may update host metadata or
/// enforce write protection, but may not modify other guest bytes or reenter the
/// CPU. Copy notifications retain their existing observational contract. These
/// callbacks must not unwind through generated code. Tracers requiring individual
/// reads, faulting mappings, MMIO, and relocating storage must decline this window.
#[derive(Debug, Clone, Copy)]
pub struct TrackedMem {
    /// Stable host pointer backing `base`; generated code may only read it.
    pub ptr: *const u8,
    /// First guest address covered by the window.
    pub base: u32,
    /// Number of contiguous readable bytes; windows shorter than four are ignored.
    pub len: u32,
}

/// Host-provided memory and device bus used by [`CpuCore`](crate::CpuCore).
///
/// Multi-byte values use the 68k's big-endian byte order. The six basic
/// read/write methods are required; the remaining hooks have conservative
/// defaults for functional emulators and can be overridden for precise
/// faults, timing, interrupts, caches, and batch acceleration.
pub trait AddressBus {
    /// Read one byte from `address`.
    fn read_byte(&mut self, address: u32) -> u8;
    /// Read one big-endian 16-bit word from `address`.
    fn read_word(&mut self, address: u32) -> u16;
    /// Read one big-endian 32-bit longword from `address`.
    fn read_long(&mut self, address: u32) -> u32;
    /// Write one byte to `address`.
    fn write_byte(&mut self, address: u32, value: u8);
    /// Write one big-endian 16-bit word to `address`.
    fn write_word(&mut self, address: u32, value: u16);
    /// Write one big-endian 32-bit longword to `address`.
    fn write_long(&mut self, address: u32, value: u32);

    /// Snapshot host metadata for a memory-to-memory MOVE operand, after its
    /// source read and before any destination write. `bytes` is 1, 2, or 4.
    /// Addresses have the CPU address mask applied. These notifications are
    /// omitted with an enabled MMU and for accesses through `fast_mem`.
    /// A bus that needs them must not expose a fast memory window.
    ///
    /// This is observational: implementations must not change guest memory,
    /// device state, faults, or timing. Return `true` when a snapshot was
    /// captured; only then is `end_memory_copy` called, including when
    /// destination resolution/write faults. Returning `false` avoids the
    /// completion callback when the source has no metadata.
    #[inline]
    fn begin_memory_copy(&mut self, _source: u32, _bytes: u32) -> bool {
        false
    }

    /// Finish a MOVE metadata transfer. `Some` identifies a completed
    /// destination write; `None` discards the snapshot after a fault.
    /// Source metadata must be captured by `begin_memory_copy`, since source
    /// and destination may overlap. This callback must not write guest bytes.
    #[inline]
    fn end_memory_copy(&mut self, _destination: Option<u32>) {}

    /// Precise-timing callback (Part E.2): called immediately before each bus
    /// access with the number of CPU clocks of internal (non-bus) processing
    /// the core performed since its previous access. The access itself then
    /// takes the standard 4 CPU clocks of a 68000 bus cycle.
    ///
    /// Hosts that emulate surrounding hardware (DMA, video beam) advance it
    /// by `cpu_clocks` here so every access lands at the hardware-exact
    /// moment. The default is a no-op, so buses that only need functional
    /// emulation are unaffected.
    fn sync(&mut self, _cpu_clocks: u32) {}

    /// Fallible read variants used to surface bus/MMU faults to the CPU core.
    ///
    /// Default implementations delegate to the infallible variants to preserve backwards
    /// compatibility for existing buses.
    #[inline]
    fn try_read_byte(&mut self, address: u32) -> Result<u8, BusFault> {
        Ok(self.read_byte(address))
    }
    /// Fallible word read used for bus/MMU fault delivery.
    ///
    /// The default delegates to [`AddressBus::read_word`] and cannot fault.
    #[inline]
    fn try_read_word(&mut self, address: u32) -> Result<u16, BusFault> {
        Ok(self.read_word(address))
    }
    /// Fallible longword read used for bus/MMU fault delivery.
    ///
    /// The default delegates to [`AddressBus::read_long`] and cannot fault.
    #[inline]
    fn try_read_long(&mut self, address: u32) -> Result<u32, BusFault> {
        Ok(self.read_long(address))
    }
    /// Fallible byte write used for bus/MMU fault delivery.
    ///
    /// The default delegates to [`AddressBus::write_byte`] and cannot fault.
    #[inline]
    fn try_write_byte(&mut self, address: u32, value: u8) -> Result<(), BusFault> {
        self.write_byte(address, value);
        Ok(())
    }
    /// Fallible word write used for bus/MMU fault delivery.
    ///
    /// The default delegates to [`AddressBus::write_word`] and cannot fault.
    #[inline]
    fn try_write_word(&mut self, address: u32, value: u16) -> Result<(), BusFault> {
        self.write_word(address, value);
        Ok(())
    }
    /// Fallible longword write used for bus/MMU fault delivery.
    ///
    /// The default delegates to [`AddressBus::write_long`] and cannot fault.
    #[inline]
    fn try_write_long(&mut self, address: u32, value: u32) -> Result<(), BusFault> {
        self.write_long(address, value);
        Ok(())
    }

    /// Read a big-endian three-byte operand from `address`, returned in the
    /// low 24 bits.
    ///
    /// The 68020/68030 transfer an operand at the size it spans - byte, word,
    /// three-byte or long (MC68020UM 5.3.1) - so a bit field covering three
    /// bytes is a single operand transfer, not a word plus a byte. The
    /// default composes one anyway, which is correct for every bus that only
    /// moves data; hosts that bill bus cycles or count transactions should
    /// override it so the access costs what the hardware charges.
    ///
    /// The 68040/68060 bus has no three-byte encoding. A host modelling one
    /// may split the access however that processor does, provided it still
    /// touches only these three bytes.
    ///
    /// The core reaches this through [`AddressBus::try_read_three_bytes`];
    /// override that variant as well on a bus that reports faults.
    #[inline]
    fn read_three_bytes(&mut self, address: u32) -> u32 {
        let hi = self.read_word(address) as u32;
        let lo = self.read_byte(address.wrapping_add(2)) as u32;
        (hi << 8) | lo
    }

    /// Write the low 24 bits of `value` as a big-endian three-byte operand.
    ///
    /// See [`AddressBus::read_three_bytes`] for when the core uses this and
    /// why a host may want to override it.
    #[inline]
    fn write_three_bytes(&mut self, address: u32, value: u32) {
        self.write_word(address, (value >> 8) as u16);
        self.write_byte(address.wrapping_add(2), value as u8);
    }

    /// Fallible three-byte read used for bus/MMU fault delivery. **This is
    /// the variant the core calls**, so a host that bills bus cycles must
    /// override it (as well as [`AddressBus::read_three_bytes`]) for the
    /// billing to take effect.
    ///
    /// The default composes the fallible word and byte reads rather than
    /// delegating to the infallible variant, so an existing bus keeps
    /// reporting a fault on whichever half of the operand fails.
    #[inline]
    fn try_read_three_bytes(&mut self, address: u32) -> Result<u32, BusFault> {
        let hi = self.try_read_word(address)? as u32;
        let lo = self.try_read_byte(address.wrapping_add(2))? as u32;
        Ok((hi << 8) | lo)
    }

    /// Fallible three-byte write used for bus/MMU fault delivery.
    ///
    /// See [`AddressBus::try_read_three_bytes`]: this is the variant the core
    /// calls, and the default composes the fallible word and byte writes.
    #[inline]
    fn try_write_three_bytes(&mut self, address: u32, value: u32) -> Result<(), BusFault> {
        self.try_write_word(address, (value >> 8) as u16)?;
        self.try_write_byte(address.wrapping_add(2), value as u8)
    }

    /// Read an instruction-stream word.
    ///
    /// Override this when opcode/immediate fetches use a distinct bus path.
    fn read_immediate_word(&mut self, address: u32) -> u16 {
        self.read_word(address)
    }
    /// Read an instruction-stream longword.
    ///
    /// Override this when opcode/immediate fetches use a distinct bus path.
    fn read_immediate_long(&mut self, address: u32) -> u32 {
        self.read_long(address)
    }
    /// Instruction-stream reads with bus-fault reporting, used by the
    /// non-prefetch (68010+) opcode/immediate path so hosts can tell
    /// fetches from data reads (e.g. to model a 32-bit fetch path).
    #[inline]
    fn try_read_immediate_word(&mut self, address: u32) -> Result<u16, BusFault> {
        Ok(self.read_immediate_word(address))
    }
    /// Fallible instruction-stream longword read.
    ///
    /// The default delegates to [`AddressBus::read_immediate_long`] and
    /// cannot fault.
    #[inline]
    fn try_read_immediate_long(&mut self, address: u32) -> Result<u32, BusFault> {
        Ok(self.read_immediate_long(address))
    }
    /// Whether the most recent instruction-stream read was served from the
    /// CPU's instruction cache. The 68060 timing model gates superscalar
    /// pairing and branch folding on a cached fetch stream; plain test
    /// buses default to true (pair freely).
    fn last_fetch_was_cached(&self) -> bool {
        true
    }

    /// Start tracking cache residency for one complete instruction. The
    /// MC68020 timing tables define their cache case for an instruction that
    /// is in the cache, including extension and immediate words rather than
    /// only the opcode word. Hosts with an instruction-cache model use this
    /// hook to reset their per-instruction hit accumulator.
    fn begin_instruction_fetches(&mut self) {}

    /// Whether every instruction-stream access since
    /// `begin_instruction_fetches` hit the instruction cache. Functional test
    /// buses without a cache model default to the most recent fetch result.
    fn instruction_fetches_were_cached(&self) -> bool {
        self.last_fetch_was_cached()
    }

    /// Synchronize the embedder at a batch instruction boundary.
    ///
    /// `retired` is the cumulative number of instructions completed in this
    /// `run_batch` call. Native traces may retire many instructions between
    /// calls, bounded by the caller's instruction budget. This hook runs before
    /// interpreted bus accesses and after native traces return. It must not
    /// relocate or resize any exposed memory window. Returning true requests
    /// a host boundary before the next instruction; completed work is retained.
    /// The default is a no-op, compiled away for ordinary functional buses.
    #[inline]
    fn batch_boundary(&mut self, _retired: u32, _pc: u32, _sr: u16) -> bool {
        false
    }

    /// Take a pending request to return from cycle-scheduled execution at
    /// the next completed instruction or interrupt-entry boundary.
    ///
    /// [`CpuCore::run_for_cycles`](crate::CpuCore::run_for_cycles) and
    /// [`CpuCore::run_for_cycles_with_hook`](crate::CpuCore::run_for_cycles_with_hook)
    /// and
    /// [`CpuCore::run_for_cycles_with_boundary_hook`](crate::CpuCore::run_for_cycles_with_boundary_hook)
    /// call this after an instruction completes normally or an entry interrupt
    /// is serviced, and before fetching or executing another instruction.
    /// Returning `true` makes the runner exit with
    /// [`CycleBatchExit::BoundaryRequested`](crate::CycleBatchExit::BoundaryRequested).
    /// Completed work is included in the result: an instruction contributes
    /// its cycles and retirement count, while interrupt entry contributes its
    /// cycles without incrementing the retirement count.
    ///
    /// Implementations should consume the request when returning `true`, so
    /// execution can resume without a separate acknowledgement call. A bus
    /// can retain any associated host work in its own queue until the caller
    /// processes the boundary exit. The default reports no request.
    #[inline]
    fn take_boundary_request(&mut self) -> bool {
        false
    }

    /// Perform an interrupt-acknowledge cycle for `level`.
    ///
    /// Return an explicit vector number in the low byte, or `u32::MAX` for
    /// the level's autovector. The default requests an autovector.
    fn interrupt_acknowledge(&mut self, _level: u8) -> u32 {
        0xFFFF_FFFF
    }

    /// IPL poll-point marker. The 68000/68010 sample their IPL pins at ONE
    /// microcode-determined point per instruction, and the take-interrupt
    /// decision at the next instruction boundary consumes that sample. A
    /// timing-accurate host latches the IPL level at the start of every bus
    /// access and, by default, lets the instruction's LAST access provide
    /// the boundary sample. For instructions whose poll point is NOT the
    /// last access (e.g. read-modify-write instructions poll during the
    /// final prefetch that precedes the writeback), the core calls this
    /// right after the polling access: the host must keep that access's
    /// sample and ignore later accesses until the boundary decision
    /// consumes it. Functional-only buses can ignore it.
    fn ipl_hold_sample(&mut self) {}

    /// MC68000 memory TST poll point. Timing-aware buses may qualify the
    /// operand-word IPL sample differently from the ordinary poll hold (for
    /// example, where a trailing-prefetch IPL qualifier does not apply).
    /// The default preserves existing host behavior.
    #[inline]
    fn ipl_hold_operand_sample(&mut self) {
        self.ipl_hold_sample();
    }

    /// Sample IPL at the current internal-cycle boundary and hold that fresh
    /// sample for the next instruction-boundary interrupt decision. Unlike
    /// `ipl_hold_sample`, this must not reuse a preceding bus access's sample.
    /// Subsequent accesses must not replace it until the host consumes it or
    /// the core calls `ipl_release_sample` during exception dispatch.
    ///
    /// The MC68000 uses this for DIVS with a nonnegative dividend and negative
    /// divisor (except early absolute overflow): IPL is sampled two CPU clocks
    /// before the final prefetch, without moving that prefetch. The core flushes
    /// internal clocks through `sync` before calling this method. Timing-aware
    /// hosts must implement the fresh sample and hold; functional buses may
    /// keep the default no-op.
    fn ipl_poll_internal(&mut self) {}

    /// Release an `ipl_hold_sample` poll-point hold before the instruction
    /// boundary consumes it. Called on exception dispatch: the vector jump's
    /// handler-entry prefetch is a fresh poll point on real silicon (Moira
    /// jumpToVector polls during the final refill read), so a hold placed
    /// earlier in the faulted instruction must not survive into the handler.
    fn ipl_release_sample(&mut self) {}

    /// Notify attached devices that the CPU asserted the external RESET line.
    fn reset_devices(&mut self) {}

    /// Act on a cache-control change made inside
    /// [`CpuCore::run_batch`](crate::CpuCore::run_batch).
    ///
    /// The core keeps CACR in `CpuCore::cacr` and latches the write-only
    /// clear strobes (CI/CEI/CD/CED, and the 68040/060 CINV/CPUSH, which
    /// surface as CI/CD) in `CpuCore::cacr_pending_ops` for a host that
    /// models the caches. A host that syncs its model only between calls
    /// into the core is too late inside a batch: the CPU acts on an
    /// invalidation before its next fetch, so code rewritten and run right
    /// after a cache clear would be served stale lines for the rest of the
    /// batch. `run_batch` therefore calls this as soon as an instruction
    /// changes the stored CACR or adds a strobe to `cacr_pending_ops` (a
    /// MOVEC to CACR, a CINV or a CPUSH), before anything else is fetched.
    /// `cacr` is the stored CACR, `caar` the CAAR, and `strobes` every
    /// strobe pending in `cacr_pending_ops`.
    ///
    /// Return `true` once the strobes have been acted on; the core then
    /// clears `cacr_pending_ops`. The default returns `false` and leaves
    /// them pending for the host to drain after the batch. The other entry
    /// points never call this: they return to the host after every
    /// instruction or cycle budget, where it can sync directly.
    #[inline]
    fn cache_control(&mut self, _cacr: u32, _caar: u32, _strobes: u32) -> bool {
        false
    }

    /// Expose a direct window into contiguous, side-effect-free guest RAM.
    ///
    /// See [`FastMem`] for the exact contract. Returning `Some` lets
    /// [`CpuCore::run_batch`](crate::CpuCore::run_batch) execute
    /// memory-operand instructions and opcode fetches without a bus call
    /// per access — typically a large speedup for memory-heavy guest
    /// code. The default returns `None` (no fast path); cycle-accurate
    /// entry points (`execute`/`step`) never use the window either way.
    #[inline]
    fn fast_mem(&mut self) -> Option<FastMem> {
        None
    }

    /// Additional plain RAM/ROM windows, searched only after the primary
    /// window misses. If there is no primary window, the first valid region
    /// becomes it (retaining read-only protection). TrackedMem is used only
    /// when neither this method nor fast_mem exposes a valid region.
    #[inline]
    fn fast_mem_regions(&mut self) -> [Option<FastMemRegion>; MAX_FAST_MEM_REGIONS] {
        [None; MAX_FAST_MEM_REGIONS]
    }

    /// Optionally expose read-direct RAM with intercepted writes. Native traces
    /// use this only when `fast_mem` is unavailable. Interpreter memory stores
    /// and compiled stores both retain bus write and MOVE metadata notifications.
    /// See [`TrackedMem`] for the required, stronger-than-ordinary-bus contract.
    fn tracked_mem(&mut self) -> Option<TrackedMem> {
        None
    }

    /// Optional inline store filter for the [`TrackedMem`] window returned by
    /// the same batch. Null (the default) sends every compiled store through
    /// the write callbacks, as before.
    ///
    /// A non-null pointer addresses bytes the bus owns: byte 0 is a global
    /// flag, and byte `1 + (address >> 12)` describes the 4 KiB guest page
    /// holding `address`, for every page of the window (plus one byte beyond
    /// it). Generated code reads these bytes on every tracked store. When the
    /// global byte and the bytes of every page a store touches (and, for a
    /// memory-to-memory copy, every page its source touches) are all zero, it
    /// writes the value directly to window RAM in big-endian order and does
    /// **not** call the bus. Otherwise it calls the bus exactly as without a
    /// filter.
    ///
    /// The bus must therefore keep a byte zero only while a direct store to
    /// that page would be indistinguishable from its own `write_*` and copy
    /// notifications: no metadata to update, no protection to enforce, no
    /// journal to record. It may change bytes at any time, including inside a
    /// write callback, and the next compiled store observes the change. The
    /// allocation must stay valid and fixed for the whole `run_batch`. A
    /// filter is used only with a `TrackedMem` window whose `base` is zero.
    fn tracked_store_filter(&mut self) -> *const u8 {
        std::ptr::null()
    }
}

/// Optional companion trait for buses that version instruction-visible memory.
///
/// This is intentionally separate from `AddressBus`: adding methods to the hot bus trait changes
/// code generation for opcode fetches in release builds. Embedders can use it
/// to coordinate external code caches without slowing buses that need only
/// [`AddressBus`].
pub trait InstructionCacheBus: AddressBus {
    /// Stable version for instruction memory at `address`, when known.
    ///
    /// Returning `Some(version)` tells an instruction cache that fetches from
    /// this address may be reused until the version changes. Returning `None`
    /// is conservative.
    #[inline]
    fn instruction_cache_version(&mut self, _address: u32) -> Option<u64> {
        None
    }

    /// Notify the bus that bytes in a code-visible range were written by the CPU.
    ///
    /// Buses that implement `instruction_cache_version` should update the relevant version here.
    #[inline]
    fn invalidate_instruction_cache(&mut self, _address: u32, _len: u32) {}
}

/// Fast linear-memory bus for RAM-backed emulators and WebAssembly builds.
///
/// This keeps all normal memory accesses inside Rust/wasm linear memory instead of crossing into a
/// host callback for each byte/word/long. Addresses wrap within the backing buffer, with a fast mask
/// path for power-of-two sizes.
#[derive(Debug, Clone)]
pub struct LinearMemoryBus {
    memory: Vec<u8>,
    wrap_mask: usize,
    power_of_two_len: bool,
    instruction_version: u64,
}

impl LinearMemoryBus {
    /// Create a zero-filled bus with `size` bytes.
    pub fn new(size: usize) -> Self {
        Self::from_vec(vec![0; size])
    }

    /// Create a bus using an existing memory buffer.
    pub fn from_vec(memory: Vec<u8>) -> Self {
        assert!(
            !memory.is_empty(),
            "LinearMemoryBus requires non-empty memory"
        );
        let power_of_two_len = memory.len().is_power_of_two();
        let wrap_mask = memory.len().saturating_sub(1);
        Self {
            memory,
            wrap_mask,
            power_of_two_len,
            instruction_version: 1,
        }
    }

    #[inline]
    /// Return the number of bytes in the backing buffer.
    pub fn len(&self) -> usize {
        self.memory.len()
    }

    #[inline]
    /// Return whether the backing buffer is empty.
    ///
    /// A constructed `LinearMemoryBus` is never empty because
    /// [`LinearMemoryBus::from_vec`] rejects an empty buffer.
    pub fn is_empty(&self) -> bool {
        self.memory.is_empty()
    }

    #[inline]
    /// Borrow the complete backing buffer.
    pub fn as_slice(&self) -> &[u8] {
        &self.memory
    }

    #[inline]
    /// Mutably borrow the complete backing buffer.
    ///
    /// Taking a mutable slice advances the instruction-memory version
    /// conservatively because the caller may change executable bytes.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.bump_instruction_version();
        &mut self.memory
    }

    /// Copy bytes into memory at `address`, wrapping at the end of the backing buffer.
    pub fn load(&mut self, address: u32, data: &[u8]) {
        if self.memory.is_empty() {
            return;
        }
        for (offset, value) in data.iter().copied().enumerate() {
            let idx = self.index(address.wrapping_add(offset as u32));
            self.memory[idx] = value;
        }
        self.bump_instruction_version();
    }

    #[inline]
    /// Write one big-endian word, wrapping within the backing buffer.
    pub fn write_word_at(&mut self, address: u32, value: u16) {
        self.write_word(address, value);
    }

    #[inline]
    /// Write one big-endian longword, wrapping within the backing buffer.
    pub fn write_long_at(&mut self, address: u32, value: u32) {
        self.write_long(address, value);
    }

    #[inline]
    fn index(&self, address: u32) -> usize {
        debug_assert!(!self.memory.is_empty());
        if self.power_of_two_len {
            (address as usize) & self.wrap_mask
        } else {
            (address as usize) % self.memory.len()
        }
    }

    #[inline]
    fn read_index(&self, index: usize) -> u8 {
        debug_assert!(index < self.memory.len());
        // Indices are produced by `index`, which wraps into the backing buffer.
        unsafe { *self.memory.get_unchecked(index) }
    }

    #[inline]
    fn write_index(&mut self, index: usize, value: u8) {
        debug_assert!(index < self.memory.len());
        // Indices are produced by `index`, which wraps into the backing buffer.
        unsafe {
            *self.memory.get_unchecked_mut(index) = value;
        }
    }

    #[inline]
    fn bump_instruction_version(&mut self) {
        self.instruction_version = self.instruction_version.wrapping_add(1);
        if self.instruction_version == 0 {
            self.instruction_version = 1;
        }
    }
}

impl AddressBus for LinearMemoryBus {
    #[inline]
    fn read_byte(&mut self, address: u32) -> u8 {
        let idx = self.index(address);
        self.read_index(idx)
    }

    /// The whole backing buffer is side-effect-free RAM, so expose it as a
    /// fastmem window starting at guest address 0. Accesses beyond `len`
    /// (which the bus methods wrap) simply fall back to the bus.
    #[inline]
    fn fast_mem(&mut self) -> Option<FastMem> {
        Some(FastMem {
            ptr: self.memory.as_mut_ptr(),
            base: 0,
            len: u32::try_from(self.memory.len()).unwrap_or(u32::MAX),
        })
    }

    #[inline]
    fn read_word(&mut self, address: u32) -> u16 {
        let b0 = self.read_index(self.index(address));
        let b1 = self.read_index(self.index(address.wrapping_add(1)));
        ((b0 as u16) << 8) | b1 as u16
    }

    #[inline]
    fn read_long(&mut self, address: u32) -> u32 {
        let b0 = self.read_index(self.index(address));
        let b1 = self.read_index(self.index(address.wrapping_add(1)));
        let b2 = self.read_index(self.index(address.wrapping_add(2)));
        let b3 = self.read_index(self.index(address.wrapping_add(3)));
        ((b0 as u32) << 24) | ((b1 as u32) << 16) | ((b2 as u32) << 8) | b3 as u32
    }

    #[inline]
    fn write_byte(&mut self, address: u32, value: u8) {
        let idx = self.index(address);
        self.write_index(idx, value);
        self.bump_instruction_version();
    }

    #[inline]
    fn write_word(&mut self, address: u32, value: u16) {
        let idx0 = self.index(address);
        let idx1 = self.index(address.wrapping_add(1));
        self.write_index(idx0, (value >> 8) as u8);
        self.write_index(idx1, value as u8);
        self.bump_instruction_version();
    }

    #[inline]
    fn write_long(&mut self, address: u32, value: u32) {
        let idx0 = self.index(address);
        let idx1 = self.index(address.wrapping_add(1));
        let idx2 = self.index(address.wrapping_add(2));
        let idx3 = self.index(address.wrapping_add(3));
        self.write_index(idx0, (value >> 24) as u8);
        self.write_index(idx1, (value >> 16) as u8);
        self.write_index(idx2, (value >> 8) as u8);
        self.write_index(idx3, value as u8);
        self.bump_instruction_version();
    }

    #[inline]
    fn read_immediate_word(&mut self, address: u32) -> u16 {
        self.read_word(address)
    }

    #[inline]
    fn read_immediate_long(&mut self, address: u32) -> u32 {
        self.read_long(address)
    }
}

impl InstructionCacheBus for LinearMemoryBus {
    #[inline]
    fn instruction_cache_version(&mut self, _address: u32) -> Option<u64> {
        Some(self.instruction_version)
    }

    #[inline]
    fn invalidate_instruction_cache(&mut self, _address: u32, _len: u32) {
        self.bump_instruction_version();
    }
}
