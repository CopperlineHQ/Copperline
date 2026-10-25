//! Guest accesses through the batch fastmem window.
//!
//! [`CpuCore::run_batch`] installs a host window of guest RAM
//! (`fm_ptr`/`fm_base`/`fm_len`, see [`crate::FastMem`]) that its decoded-op,
//! memory-op and trace paths read and write directly. The window is indexed
//! by physical address. With the PMMU disabled every guest address is its own
//! physical address. With it enabled each access is translated first, by
//! [`crate::mmu::probe_resident`]: the transparent-translation registers and
//! the ATC, never a table search. Whatever they cannot answer -- an ATC
//! miss, an access the cached translation would fault, a physical address
//! outside the window, an access spanning two pages -- comes back `None`,
//! and the caller runs the whole instruction through the ordinary bus path,
//! which searches the tables and raises any fault with its exact frame.
//!
//! Translating through the live ATC (rather than a copy of it) keeps every
//! PFLUSH, PMOVE, MOVEC and reset in force with no invalidation of its own:
//! an entry the guest flushed or the ATC replaced simply misses.

use super::cpu::CpuCore;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Region {
    pub ptr: usize,
    pub base: u32,
    pub len: u32,
    pub writable: bool,
}

impl Region {
    pub fn covers(self, addr: u32, bytes: u32) -> bool {
        bytes != 0 && bytes <= self.len && addr.wrapping_sub(self.base) <= self.len - bytes
    }
}

/// `fm_code_page` when no program-space translation is remembered: page
/// bases are even.
pub(crate) const NO_CODE_PAGE: u32 = 1;

impl CpuCore {
    /// Capture the PMMU state the window translates with, or none when no
    /// window is installed or the PMMU is disabled. `run_batch` calls this
    /// when it installs the window and after every instruction it fully
    /// dispatches, the only kind that can write TC or a TT register.
    #[inline]
    pub(crate) fn refresh_window_mmu(&mut self) {
        self.fm_mmu = if self.fm_len != 0 {
            crate::mmu::ResidentMmu::of(self)
        } else {
            crate::mmu::ResidentMmu::default()
        };
        self.forget_code_page();
    }

    /// Drop the remembered program-space translation. The decoded batch
    /// loop calls this on entry and after any access that took the bus
    /// path, the only place it could have changed the ATC (a table search
    /// loads an entry, possibly replacing the remembered one).
    #[inline(always)]
    pub(crate) fn forget_code_page(&mut self) {
        self.fm_code_page = NO_CODE_PAGE;
    }

    /// Translated window offset of the instruction word at the even,
    /// address-masked `addr`, for the decoded batch loop's opcode fetch.
    /// Consecutive fetches mostly share a page, so the loop remembers the
    /// last page it translated: while the ATC, TC, the TT registers and the
    /// privilege level stay as they were (they do within one run of the
    /// loop, which forgets the page whenever it leaves its window paths),
    /// every address in it translates the same way.
    #[inline(always)]
    pub(crate) fn code_window_off(&mut self, addr: u32) -> Option<u32> {
        debug_assert!(self.fm_mmu.enabled());
        match self.remembered_code_off(addr) {
            Some(off) => Some(off),
            None => self.remember_code_page(addr),
        }
    }

    /// The window offset of `addr` through the remembered program-space
    /// translation, if `addr` lies in that page and the window.
    #[inline(always)]
    pub(crate) fn remembered_code_off(&self, addr: u32) -> Option<u32> {
        if addr & !self.fm_mmu.page_mask != self.fm_code_page {
            return None;
        }
        // The page may run past the end of the window.
        let off = addr.wrapping_add(self.fm_code_delta);
        let off = self.physical_window_off(self.fm_base.wrapping_add(off), 2, false);
        debug_assert_eq!(off, self.translated_window_off(addr, 2, false, true));
        off
    }

    #[inline(never)]
    fn remember_code_page(&mut self, addr: u32) -> Option<u32> {
        let off = self.translated_window_off(addr, 2, false, true)?;
        self.fm_code_page = addr & !self.fm_mmu.page_mask;
        self.fm_code_delta = off.wrapping_sub(addr);
        Some(off)
    }

    /// Window offset of a `bytes`-byte (1 to 4) access at the address-masked
    /// `addr`, or `None` when the access must go through the bus. `write`
    /// asks for write permission (a read-modify-write needs it too) and
    /// `instruction` selects the program space; both matter only with the
    /// PMMU enabled. Used by the portable trace executor.
    #[cfg(any(not(feature = "jit"), target_family = "wasm", test))]
    #[inline(always)]
    pub(crate) fn window_off(
        &self,
        addr: u32,
        bytes: u32,
        write: bool,
        instruction: bool,
    ) -> Option<u32> {
        debug_assert!((1..=4).contains(&bytes));
        debug_assert!(self.fm_len >= 4, "window_off without an installed window");
        if self.fm_mmu.enabled() {
            return self.translated_window_off(addr, bytes, write, instruction);
        }
        self.physical_window_off(addr, bytes, write)
    }

    /// [`Self::window_off`] for a data transfer of any length (the portable
    /// trace executor's MOVEM).
    #[cfg(any(not(feature = "jit"), target_family = "wasm", test))]
    #[inline(always)]
    pub(crate) fn window_range_off(&self, addr: u32, bytes: u32, write: bool) -> Option<u32> {
        if self.fm_mmu.enabled() {
            return self.translated_window_off(addr, bytes, write, false);
        }
        self.physical_window_off(addr, bytes, write)
    }

    /// Window offset of a `bytes`-byte access at the logical, address-masked
    /// `addr` with the PMMU enabled: translate it, then locate the physical
    /// bytes in the window (`None` sends the access to the bus). Kept out of
    /// line so the untranslated callers inline as compactly as before.
    #[inline(never)]
    pub(crate) fn translated_window_off(
        &self,
        addr: u32,
        bytes: u32,
        write: bool,
        instruction: bool,
    ) -> Option<u32> {
        let mmu = self.fm_mmu;
        // The interpreter runs an access that spans two pages as one bus
        // cycle per page, each translated (and faulting) on its own.
        if (addr & mmu.page_mask).wrapping_add(bytes - 1) > mmu.page_mask {
            return None;
        }
        // MOVES translates in another space, and a 68030 bus-fault RTE with
        // DF cleared hands the interpreter a one-shot read result or write
        // discard for a data address; leave all of them to it.
        if self.mmu_fc_override.is_some() {
            return None;
        }
        if !instruction
            && (self
                .mmu_read_override
                .is_some_and(|(at, _)| at.wrapping_sub(addr) < bytes)
                || self
                    .mmu_write_suppress
                    .is_some_and(|at| at.wrapping_sub(addr) < bytes))
        {
            return None;
        }
        let phys = crate::mmu::probe_resident(self, mmu, addr, write, instruction)?;
        self.physical_window_off(self.address(phys), bytes, write)
    }
}

/// Generated-trace entry to [`CpuCore::translated_window_off`] for a data
/// access. `request` holds the access size in bytes (bits 7:0) and the write
/// flag (bit 8); the result is the window offset, or `u32::MAX` to bail
/// (no offset can be: an access of at least one byte ends inside the
/// window).
#[cfg(all(feature = "jit", not(target_family = "wasm")))]
pub(crate) extern "C" fn trace_window_off(cpu: *const CpuCore, addr: u32, request: u32) -> u32 {
    // SAFETY: a generated trace passes the CpuCore it was entered with,
    // which nothing writes while this call reads it.
    let cpu = unsafe { &*cpu };
    cpu.window_access_off(addr, request & 0xFF, request & 0x100 != 0, false)
        .unwrap_or(u32::MAX)
}

impl CpuCore {
    pub(crate) fn capture_extra_window(&mut self, fm: super::memory::FastMemRegion) {
        let end = u64::from(fm.base) + u64::from(fm.len);
        let ptr = fm.ptr as usize;
        let cpu = self as *const Self as usize;
        let cpu_end = cpu + std::mem::size_of::<Self>();
        if ptr == 0
            || fm.len < 4
            || end > (1u64 << 32)
            || ptr.checked_add(fm.len as usize).is_none()
            || (ptr < cpu_end && cpu < ptr + fm.len as usize)
        {
            return;
        }
        let overlaps = |base: u32, len: u32| {
            len != 0
                && u64::from(fm.base) < u64::from(base) + u64::from(len)
                && u64::from(base) < end
        };
        if overlaps(self.fm_base, self.fm_len)
            || self.fm_regions[..self.fm_region_count]
                .iter()
                .any(|r| overlaps(r.base, r.len))
        {
            return;
        }
        if self.fm_len == 0 {
            self.fm_ptr = ptr;
            self.fm_base = fm.base;
            self.fm_len = fm.len;
            self.fm_readonly = !fm.writable;
        } else {
            self.fm_regions[self.fm_region_count] = Region {
                ptr,
                base: fm.base,
                len: fm.len,
                writable: fm.writable,
            };
            self.fm_region_count += 1;
        }
        self.trace_record_skip = [super::trace_jit::TRACE_PC_NONE; 4];
        self.trace_probe_skip = [super::trace_jit::TRACE_PC_NONE; 4];
    }

    /// An offset token is a physical address minus fm_base, wrapping at 32 bits.
    /// It is an allocation offset ONLY when it lies in the primary window.
    /// Every consumer must resolve it with window_host_ptr before dereferencing.
    #[inline]
    pub(crate) fn physical_window_off(&self, addr: u32, bytes: u32, write: bool) -> Option<u32> {
        let off = addr.wrapping_sub(self.fm_base);
        if bytes != 0 && bytes <= self.fm_len && off <= self.fm_len - bytes {
            return (!write || !self.fm_readonly).then_some(off);
        }
        // u32::MAX is the native helper's miss sentinel. An access at exactly
        // fm_base-1 remains correct through the bus rather than being accelerated.
        if off == u32::MAX {
            return None;
        }
        self.fm_regions[..self.fm_region_count]
            .iter()
            .find(|r| r.covers(addr, bytes))
            .filter(|r| !write || r.writable)
            .map(|_| off)
    }

    #[inline]
    pub(crate) fn window_access_off(
        &self,
        addr: u32,
        bytes: u32,
        write: bool,
        instruction: bool,
    ) -> Option<u32> {
        if self.fm_mmu.enabled() {
            self.translated_window_off(addr, bytes, write, instruction)
        } else {
            self.physical_window_off(addr, bytes, write)
        }
    }

    /// Resolve a previously range- and permission-checked physical token.
    #[inline]
    pub(crate) fn window_host_ptr(&self, off: u32) -> *mut u8 {
        if off < self.fm_len {
            // SAFETY: every caller has validated the complete access range.
            return unsafe { (self.fm_ptr as *mut u8).add(off as usize) };
        }
        let addr = self.fm_base.wrapping_add(off);
        self.fm_regions[..self.fm_region_count]
            .iter()
            .find(|r| r.covers(addr, 1))
            .map_or(std::ptr::null_mut(), |r| unsafe {
                (r.ptr as *mut u8).add(addr.wrapping_sub(r.base) as usize)
            })
    }
}

#[cfg(all(feature = "jit", not(target_family = "wasm")))]
pub(crate) extern "C" fn trace_window_ptr(cpu: *const CpuCore, off: u32) -> usize {
    // SAFETY: generated code owns the live CpuCore and has preflighted this token.
    unsafe { (&*cpu).window_host_ptr(off) as usize }
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;
