// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared-buffer heap: ALLOC_SHARED/FREE_SHARED over the Amiga-visible
//! window region (board offset 0x10000 up to the board size).
//!
//! First-fit with coalescing free, 16-byte minimum alignment, and
//! generation-tagged handles so a stale handle after free reports
//! BAD_HANDLE instead of aliasing a new allocation. A handle is
//! `(generation << 16) | (slot + 1)` with the generation capped to 15 bits,
//! so it can never be 0 (the tools' "was this allocated" guard) nor
//! 0xFFFFFFFF (ZZ9K_INVALID_HANDLE).

use crate::wire::{AMIGA_MEMORY_OFFSET, MAX_SHARED_BUFFERS};

const MIN_ALIGN: u32 = 16;

#[derive(Clone, Copy)]
struct Slot {
    off: u32,
    len: u32,
    generation: u16,
}

pub struct Heap {
    base: u32,
    limit: u32,
    /// Free regions, sorted by offset, never adjacent (coalesced on free).
    free: Vec<(u32, u32)>,
    slots: Vec<Option<Slot>>,
    next_generation: u16,
}

impl Heap {
    pub fn new(board_size: u32) -> Self {
        let base = AMIGA_MEMORY_OFFSET;
        let limit = board_size.max(base);
        Heap {
            base,
            limit,
            free: vec![(base, limit - base)],
            slots: vec![None; MAX_SHARED_BUFFERS as usize],
            next_generation: 1,
        }
    }

    fn bump_generation(&mut self) -> u16 {
        let generation = self.next_generation;
        // Cap to 15 bits and skip 0 so the encoded handle stays clear of 0
        // and 0xFFFFFFFF for every slot index.
        self.next_generation = if generation >= 0x7FFE {
            1
        } else {
            generation + 1
        };
        generation
    }

    fn encode(slot: usize, generation: u16) -> u32 {
        (u32::from(generation) << 16) | (slot as u32 + 1)
    }

    fn decode(&self, handle: u32) -> Option<usize> {
        let slot = (handle & 0xFFFF).checked_sub(1)? as usize;
        let generation = (handle >> 16) as u16;
        match self.slots.get(slot)? {
            Some(s) if s.generation == generation => Some(slot),
            _ => None,
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
        let slot = self.slots.iter().position(|s| s.is_none())?;
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
            let generation = self.bump_generation();
            self.slots[slot] = Some(Slot {
                off: aligned,
                len,
                generation,
            });
            return Some((Self::encode(slot, generation), aligned, len));
        }
        None
    }

    /// Free a handle; false = stale or never allocated (BAD_HANDLE).
    pub fn free(&mut self, handle: u32) -> bool {
        let Some(slot) = self.decode(handle) else {
            return false;
        };
        let Slot { off, len, .. } = self.slots[slot].take().unwrap();
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
        let s = self.slots[slot].as_ref().unwrap();
        if offset > s.len || len > s.len - offset {
            return None;
        }
        Some((s.off + offset, len))
    }

    pub fn buffers_used(&self) -> u32 {
        self.slots.iter().filter(|s| s.is_some()).count() as u32
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
