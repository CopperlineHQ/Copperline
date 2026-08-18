// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared-buffer heap: ALLOC_SHARED/FREE_SHARED over the Amiga-visible
//! window region (board offset 0x10000 up to the board size).
//!
//! First-fit with coalescing free, 16-byte minimum alignment, and
//! generation-tagged handles so a stale handle after free reports
//! BAD_HANDLE instead of aliasing a new allocation. A handle is
//! `(generation << 7) | (slot + 1)`: the slot index fits 7 bits (64 slots)
//! and the per-slot generation gets the remaining 25, so a stale handle
//! could only revalidate after ~33 million reuses of one slot -- and a
//! slot whose generation would wrap is retired instead, making handle
//! reuse impossible outright. A handle can never be 0 (the tools' "was
//! this allocated" guard, slot + 1 >= 1) nor 0xFFFFFFFF
//! (ZZ9K_INVALID_HANDLE: the low 7 bits would need to be 0x7F = slot 126,
//! beyond the 64-slot table).

use crate::wire::{AMIGA_MEMORY_OFFSET, MAX_SHARED_BUFFERS};

const MIN_ALIGN: u32 = 16;

/// Highest generation a slot may reach; a slot at the ceiling is retired
/// rather than wrapped, so a generation value is never reused for a slot.
const GENERATION_MAX: u32 = (1 << 25) - 1;

#[derive(Clone, Copy)]
struct Slot {
    off: u32,
    len: u32,
}

/// One entry per slot index: the slot's live allocation (if any) plus its
/// monotonically increasing generation. `retired` slots are never
/// allocated from again.
#[derive(Clone, Copy)]
struct SlotState {
    live: Option<Slot>,
    generation: u32,
    retired: bool,
}

pub struct Heap {
    base: u32,
    limit: u32,
    /// Free regions, sorted by offset, never adjacent (coalesced on free).
    free: Vec<(u32, u32)>,
    slots: Vec<SlotState>,
}

impl Heap {
    pub fn new(board_size: u32) -> Self {
        let base = AMIGA_MEMORY_OFFSET;
        let limit = board_size.max(base);
        Heap {
            base,
            limit,
            free: vec![(base, limit - base)],
            slots: vec![
                SlotState {
                    live: None,
                    generation: 1,
                    retired: false,
                };
                MAX_SHARED_BUFFERS as usize
            ],
        }
    }

    fn encode(slot: usize, generation: u32) -> u32 {
        (generation << 7) | (slot as u32 + 1)
    }

    fn decode(&self, handle: u32) -> Option<usize> {
        let slot = (handle & 0x7F).checked_sub(1)? as usize;
        let generation = handle >> 7;
        let state = self.slots.get(slot)?;
        if state.live.is_some() && state.generation == generation {
            Some(slot)
        } else {
            None
        }
    }

    /// Allocate `len` bytes at `alignment` (power of two; anything under 16
    /// is raised to 16). Returns (handle, board_offset, rounded_len).
    pub fn alloc(&mut self, len: u32, alignment: u32) -> Option<(u32, u32, u32)> {
        if len == 0 {
            return None;
        }
        let align = alignment.max(MIN_ALIGN);
        if !align.is_power_of_two() {
            return None;
        }
        let len = len.checked_add(MIN_ALIGN - 1)? & !(MIN_ALIGN - 1);
        let slot = self
            .slots
            .iter()
            .position(|s| s.live.is_none() && !s.retired)?;
        for i in 0..self.free.len() {
            let (off, avail) = self.free[i];
            let aligned = off.checked_add(align - 1)? & !(align - 1);
            let pad = aligned - off;
            if avail < pad || avail - pad < len {
                continue;
            }
            // Carve [aligned, aligned+len) out of the region, keeping any
            // leading pad and trailing remainder on the free list.
            self.free.remove(i);
            let mut insert_at = i;
            if pad != 0 {
                self.free.insert(insert_at, (off, pad));
                insert_at += 1;
            }
            let rest = avail - pad - len;
            if rest != 0 {
                self.free.insert(insert_at, (aligned + len, rest));
            }
            self.slots[slot].live = Some(Slot { off: aligned, len });
            let generation = self.slots[slot].generation;
            return Some((Self::encode(slot, generation), aligned, len));
        }
        None
    }

    /// Free a handle; false = stale or never allocated (BAD_HANDLE).
    pub fn free(&mut self, handle: u32) -> bool {
        let Some(slot) = self.decode(handle) else {
            return false;
        };
        let Slot { off, len } = self.slots[slot].live.take().unwrap();
        // Advance the slot's generation so this handle is stale forever;
        // a slot that would wrap is retired from further allocation
        // instead, keeping the guarantee absolute.
        if self.slots[slot].generation >= GENERATION_MAX {
            self.slots[slot].retired = true;
        } else {
            self.slots[slot].generation += 1;
        }
        let at = self.free.partition_point(|&(o, _)| o < off);
        self.free.insert(at, (off, len));
        // Coalesce with the neighbour on each side.
        if at + 1 < self.free.len() && self.free[at].0 + self.free[at].1 == self.free[at + 1].0 {
            self.free[at].1 += self.free[at + 1].1;
            self.free.remove(at + 1);
        }
        if at > 0 && self.free[at - 1].0 + self.free[at - 1].1 == self.free[at].0 {
            self.free[at - 1].1 += self.free[at].1;
            self.free.remove(at);
        }
        true
    }

    /// Resolve (handle, offset, len) to a bounds-checked board-window
    /// range. `len == 0` resolves to an empty range at any in-bounds offset.
    pub fn resolve(&self, handle: u32, offset: u32, len: u32) -> Option<(u32, u32)> {
        let slot = self.decode(handle)?;
        let s = self.slots[slot].live.as_ref().unwrap();
        if offset > s.len || len > s.len - offset {
            return None;
        }
        Some((s.off + offset, len))
    }

    pub fn buffers_used(&self) -> u32 {
        self.slots.iter().filter(|s| s.live.is_some()).count() as u32
    }

    pub fn total(&self) -> u32 {
        self.limit - self.base
    }

    pub fn free_bytes(&self) -> u32 {
        self.free.iter().map(|&(_, len)| len).sum()
    }

    pub fn largest_free(&self) -> u32 {
        self.free.iter().map(|&(_, len)| len).max().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_never_zero_or_invalid() {
        let mut heap = Heap::new(0x0040_0000);
        for _ in 0..1000 {
            let (handle, _, _) = heap.alloc(64, 16).unwrap();
            assert_ne!(handle, 0);
            assert_ne!(handle, 0xFFFF_FFFF);
            assert!(heap.free(handle));
        }
    }

    #[test]
    fn stale_handle_is_rejected_after_free() {
        let mut heap = Heap::new(0x0040_0000);
        let (handle, off, _) = heap.alloc(128, 16).unwrap();
        assert!(heap.free(handle));
        assert!(!heap.free(handle));
        assert!(heap.resolve(handle, 0, 1).is_none());
        // A reallocation of the same region gets a different handle.
        let (handle2, off2, _) = heap.alloc(128, 16).unwrap();
        assert_eq!(off, off2);
        assert_ne!(handle, handle2);
    }

    #[test]
    fn resolve_is_bounds_checked() {
        let mut heap = Heap::new(0x0040_0000);
        let (handle, off, len) = heap.alloc(100, 16).unwrap();
        assert_eq!(len, 112); // rounded up to the 16-byte granule
        assert_eq!(heap.resolve(handle, 0, len), Some((off, len)));
        assert_eq!(heap.resolve(handle, len, 0), Some((off + len, 0)));
        assert!(heap.resolve(handle, len, 1).is_none());
        assert!(heap.resolve(handle, 4, len).is_none());
        assert!(heap.resolve(handle, 0xFFFF_FFFF, 1).is_none());
    }

    #[test]
    fn free_coalesces_and_alignment_holds() {
        let mut heap = Heap::new(0x0040_0000);
        let total = heap.free_bytes();
        let a = heap.alloc(0x1000, 16).unwrap();
        let b = heap.alloc(0x1000, 4096).unwrap();
        let c = heap.alloc(0x1000, 16).unwrap();
        assert_eq!(b.1 % 4096, 0);
        assert!(heap.free(b.0));
        assert!(heap.free(a.0));
        assert!(heap.free(c.0));
        assert_eq!(heap.free_bytes(), total);
        assert_eq!(heap.largest_free(), total);
        assert_eq!(heap.buffers_used(), 0);
    }

    #[test]
    fn slot_reuse_never_revalidates_a_stale_handle() {
        // Per-slot generations: cycling one slot many times must never
        // hand out a handle value seen before, and every freed handle
        // stays stale against all later allocations of the same slot.
        let mut heap = Heap::new(0x0010_0000);
        let (first, _, _) = heap.alloc(64, 16).unwrap();
        assert!(heap.free(first));
        let mut seen = std::collections::HashSet::new();
        seen.insert(first);
        for _ in 0..100_000 {
            let (handle, _, _) = heap.alloc(64, 16).unwrap();
            assert!(seen.insert(handle), "handle value reused: {handle:#x}");
            assert!(heap.resolve(first, 0, 1).is_none(), "stale revalidated");
            assert!(heap.free(handle));
        }
    }

    #[test]
    fn generation_ceiling_retires_the_slot() {
        // A slot whose generation reaches the ceiling is retired rather
        // than wrapped: allocation moves on to the next slot and the old
        // handles stay stale.
        let mut heap = Heap::new(0x0010_0000);
        heap.slots[0].generation = GENERATION_MAX;
        let (h0, _, _) = heap.alloc(64, 16).unwrap();
        assert_eq!(h0 & 0x7F, 1, "slot 0 first");
        assert!(heap.free(h0));
        assert!(heap.slots[0].retired);
        let (h1, _, _) = heap.alloc(64, 16).unwrap();
        assert_eq!(h1 & 0x7F, 2, "slot 0 retired, slot 1 next");
        assert!(heap.resolve(h0, 0, 1).is_none());
        assert!(heap.free(h1));
    }

    #[test]
    fn exhaustion_reports_no_memory_not_panic() {
        let mut heap = Heap::new(AMIGA_MEMORY_OFFSET + 0x100);
        assert!(heap.alloc(0x200, 16).is_none());
        let (h, _, _) = heap.alloc(0x100, 16).unwrap();
        assert!(heap.alloc(16, 16).is_none());
        assert!(heap.free(h));
        // Slot exhaustion: at most MAX_SHARED_BUFFERS live handles.
        let mut heap = Heap::new(0x0040_0000);
        let handles: Vec<u32> = (0..MAX_SHARED_BUFFERS)
            .map(|_| heap.alloc(16, 16).unwrap().0)
            .collect();
        assert!(heap.alloc(16, 16).is_none());
        for h in handles {
            assert!(heap.free(h));
        }
    }
}
