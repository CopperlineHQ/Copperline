//! Address Translation Cache (ATC).
//!
//! The page-table walk costs several descriptor fetches per access, which is far
//! too expensive to pay on every cycle-stepped memory reference. Real 68030/68040
//! parts cache recent translations in an ATC; we model the same: a small
//! direct-mapped cache of (logical page -> physical page) keyed by the logical
//! page frame and the supervisor flag (user and supervisor can map a page
//! differently via separate root pointers).
//!
//! The ATC is a pure cache: it holds nothing that backing memory does not, so it
//! is never serialized (a save state restores it empty) and is flushed whenever
//! the mapping could change -- a write to TC / a root pointer / a TTR, or a
//! PFLUSH/PFLUSHA. Like real hardware, a plain CPU write to a page-table entry
//! does NOT auto-flush; software must PFLUSH, which is where we flush.

const ATC_ENTRIES: usize = 64;

#[derive(Debug, Clone, Copy, Default)]
struct AtcEntry {
    valid: bool,
    /// `(page_frame << 1) | supervisor`, disambiguating user vs supervisor maps.
    tag: u32,
    /// Physical page base (aligned to the page size in force at fill time).
    phys_page: u32,
    /// Write-protected page (W): a write must fault, not hit.
    write_protected: bool,
    /// Supervisor-only page (S): a user access must fault, not hit.
    supervisor_only: bool,
}

/// A direct-mapped address translation cache.
#[derive(Debug, Clone)]
pub struct Atc {
    entries: [AtcEntry; ATC_ENTRIES],
}

impl Default for Atc {
    fn default() -> Self {
        Self {
            entries: [AtcEntry::default(); ATC_ENTRIES],
        }
    }
}

impl Atc {
    #[inline]
    fn tag(page_frame: u32, supervisor: bool) -> u32 {
        (page_frame << 1) | supervisor as u32
    }

    #[inline]
    fn index(page_frame: u32) -> usize {
        (page_frame as usize) & (ATC_ENTRIES - 1)
    }

    /// Look up the physical page base for `page_frame` (logical address >> page
    /// bits) for this access, or `None` on a miss. A cached entry whose
    /// permissions the access would violate (a write to a write-protected page,
    /// or a user access to a supervisor page) returns `None` so the caller
    /// re-walks and raises the fault -- the permission check is never bypassed.
    #[inline]
    pub fn lookup(&self, page_frame: u32, supervisor: bool, write: bool) -> Option<u32> {
        let e = &self.entries[Self::index(page_frame)];
        if !e.valid || e.tag != Self::tag(page_frame, supervisor) {
            return None;
        }
        if (write && e.write_protected) || (!supervisor && e.supervisor_only) {
            return None;
        }
        Some(e.phys_page)
    }

    /// Record a freshly-walked translation and its protection bits.
    #[inline]
    pub fn insert(
        &mut self,
        page_frame: u32,
        supervisor: bool,
        phys_page: u32,
        write_protected: bool,
        supervisor_only: bool,
    ) {
        self.entries[Self::index(page_frame)] = AtcEntry {
            valid: true,
            tag: Self::tag(page_frame, supervisor),
            phys_page,
            write_protected,
            supervisor_only,
        };
    }

    /// Invalidate everything (PFLUSHA, TC/root-pointer/TTR write, reset).
    pub fn flush_all(&mut self) {
        for e in &mut self.entries {
            e.valid = false;
        }
    }

    /// Invalidate the entry for one logical page frame (PFLUSH `(An)`), both the
    /// user and supervisor variant since the same slot holds either.
    pub fn flush_page(&mut self, page_frame: u32) {
        let e = &mut self.entries[Self::index(page_frame)];
        if e.tag >> 1 == page_frame {
            e.valid = false;
        }
    }
}

/// Entries in the 68030's ATC (MC68030 User's Manual: 22 entries, fully
/// associative).
pub const ATC_030_ENTRIES: usize = 22;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
struct Atc030Entry {
    valid: bool,
    /// Function code the translation was made for. The 030 tags every entry
    /// with the access's FC, so user and supervisor, program and data spaces
    /// never share an entry (FCL and SRE can map them differently).
    fc: u8,
    /// Logical page number (`logical >> shift`).
    page: u32,
    /// Physical page base (`physical & !page_mask`).
    phys_page: u32,
    /// Write-protected page: a write must fault, so it misses and re-walks.
    write_protected: bool,
    /// Last use, for least-recently-used replacement.
    stamp: u64,
}

/// The 68030 address translation cache: 22 fully associative entries, each
/// tagged with the access's function code and logical page, holding the
/// physical page and its write protection. A hit costs the access nothing; a
/// miss is a table search whose descriptor fetches the caller pays for.
///
/// Like the silicon it keeps only resident translations of whole pages (the
/// page size TC configures, also for an early-termination descriptor that
/// maps more), is replaced least recently used first (in a `run_batch` with
/// a translated fastmem window hits record no use, so entries age by load
/// order there; see `mmu::probe_resident`), and is invalidated by
/// PFLUSH and by a PMOVE to TC, SRP or CRP without FD. A CPU write to a
/// descriptor does not reach it; software flushes, as on real hardware. A
/// write to a write-protected page, and any access that would fault, re-walk
/// the tables so the fault is raised there. Not modelled: entries for invalid
/// translations (the B bit), the CI and M bits, and the silicon's exact
/// replacement order.
///
/// It is architectural state that decides how many descriptor fetches later
/// accesses make, so it is serialized: a resumed machine misses where an
/// uninterrupted one would.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Atc030 {
    entries: [Atc030Entry; ATC_030_ENTRIES],
    /// Most recently used entry, checked first: consecutive accesses mostly
    /// fall in the same page.
    mru: u8,
    /// Use counter behind the entries' stamps.
    clock: u64,
    /// Page shift the entries were made under (0 while empty).
    shift: u8,
    /// Where each (function code, page) was last found, indexed by
    /// [`Self::hint_slot`]. Instruction fetches and data accesses use
    /// different function codes, so the MRU entry changes on almost every
    /// access; the hint finds the other entry without searching all 22.
    /// A hinted entry is checked before it is used, so a stale hint only
    /// costs the search it failed to save. It decides nothing a lookup
    /// returns or which entry an insert replaces, so it is neither
    /// serialized nor compared.
    #[cfg_attr(feature = "serde", serde(skip))]
    hints: [u8; ATC_030_HINTS],
}

/// Slots in [`Atc030`]'s lookup hint.
const ATC_030_HINTS: usize = 32;

impl PartialEq for Atc030 {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
            && self.mru == other.mru
            && self.clock == other.clock
            && self.shift == other.shift
    }
}

impl Eq for Atc030 {}

impl Default for Atc030 {
    fn default() -> Self {
        Self {
            entries: [Atc030Entry::default(); ATC_030_ENTRIES],
            mru: 0,
            clock: 0,
            shift: 0,
            hints: [0; ATC_030_HINTS],
        }
    }
}

impl Atc030 {
    #[inline]
    fn hint_slot(fc: u8, page: u32) -> usize {
        (page as usize ^ (usize::from(fc) << 2)) & (ATC_030_HINTS - 1)
    }

    /// The physical page base for `page` (logical address >> `shift`) in
    /// function-code space `fc`, or `None` on a miss. A write to a
    /// write-protected page misses, so the caller re-walks and faults.
    #[inline]
    pub fn lookup(&mut self, fc: u8, page: u32, shift: u32, write: bool) -> Option<u32> {
        if u32::from(self.shift) != shift {
            // TC changed the page size under PMOVEFD: nothing cached applies.
            return None;
        }
        let hit = |e: &Atc030Entry| e.valid && e.page == page && e.fc == fc;
        let mru = usize::from(self.mru);
        let index = if hit(&self.entries[mru]) {
            mru
        } else {
            // At most one entry holds a (function code, page), so the hint
            // finds the entry the search would.
            let slot = Self::hint_slot(fc, page);
            let hinted = usize::from(self.hints[slot]);
            let index = if hinted < ATC_030_ENTRIES && hit(&self.entries[hinted]) {
                hinted
            } else {
                let index = self.entries.iter().position(hit)?;
                self.hints[slot] = index as u8;
                index
            };
            self.clock += 1;
            self.entries[index].stamp = self.clock;
            self.mru = index as u8;
            index
        };
        let e = &self.entries[index];
        if write && e.write_protected {
            return None;
        }
        Some(e.phys_page)
    }

    /// [`Self::lookup`] without recording a use: for `run_batch` while its
    /// fastmem window translates, where no hit may record one (see
    /// `mmu::probe_resident`), and for reads made on the side, such as the
    /// trace recorder decoding code the CPU already fetched.
    #[inline]
    pub fn probe(&self, fc: u8, page: u32, shift: u32, write: bool) -> Option<u32> {
        if u32::from(self.shift) != shift {
            return None;
        }
        let hit = |e: &Atc030Entry| e.valid && e.page == page && e.fc == fc;
        let hinted = usize::from(self.hints[Self::hint_slot(fc, page)]);
        let e = if hit(&self.entries[usize::from(self.mru)]) {
            &self.entries[usize::from(self.mru)]
        } else if hinted < ATC_030_ENTRIES && hit(&self.entries[hinted]) {
            &self.entries[hinted]
        } else {
            self.entries.iter().find(|e| hit(e))?
        };
        if write && e.write_protected {
            return None;
        }
        Some(e.phys_page)
    }

    /// Record a table search's translation, replacing an invalid entry or
    /// else the least recently used one.
    pub fn insert(&mut self, fc: u8, page: u32, shift: u32, phys_page: u32, write_protected: bool) {
        if u32::from(self.shift) != shift {
            self.flush_all();
            self.shift = shift as u8;
        }
        let index = match self
            .entries
            .iter()
            .position(|e| e.valid && e.page == page && e.fc == fc)
        {
            Some(index) => index,
            None => match self.entries.iter().position(|e| !e.valid) {
                Some(index) => index,
                None => self
                    .entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| e.stamp)
                    .map_or(0, |(i, _)| i),
            },
        };
        self.clock += 1;
        self.entries[index] = Atc030Entry {
            valid: true,
            fc,
            page,
            phys_page,
            write_protected,
            stamp: self.clock,
        };
        self.mru = index as u8;
        self.hints[Self::hint_slot(fc, page)] = index as u8;
    }

    /// PFLUSHA, reset: invalidate every entry.
    pub fn flush_all(&mut self) {
        for e in &mut self.entries {
            e.valid = false;
        }
    }

    /// PFLUSH `fc,#mask`: invalidate the entries whose function code matches
    /// `fc` in the bits `mask` selects.
    pub fn flush_fc(&mut self, fc: u8, mask: u8) {
        for e in &mut self.entries {
            if (e.fc ^ fc) & mask == 0 {
                e.valid = false;
            }
        }
    }

    /// PFLUSH `fc,#mask,<ea>`: as [`Self::flush_fc`], restricted to the page
    /// holding `logical`.
    pub fn flush_fc_page(&mut self, fc: u8, mask: u8, logical: u32) {
        if self.shift == 0 {
            return;
        }
        let page = logical >> self.shift;
        for e in &mut self.entries {
            if (e.fc ^ fc) & mask == 0 && e.page == page {
                e.valid = false;
            }
        }
    }

    /// Valid entries, for tests and diagnostics.
    pub fn valid_entries(&self) -> usize {
        self.entries.iter().filter(|e| e.valid).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_after_insert_miss_after_flush() {
        let mut atc = Atc::default();
        assert_eq!(atc.lookup(0x10, false, false), None);
        atc.insert(0x10, false, 0x8000, false, false);
        assert_eq!(atc.lookup(0x10, false, false), Some(0x8000));
        // Supervisor map of the same page frame is a distinct entry.
        assert_eq!(atc.lookup(0x10, true, false), None);
        atc.flush_all();
        assert_eq!(atc.lookup(0x10, false, false), None);
    }

    #[test]
    fn flush_page_drops_only_that_frame() {
        let mut atc = Atc::default();
        atc.insert(0x10, false, 0x1000, false, false);
        atc.flush_page(0x11); // different frame, same is unlikely-index: no-op for 0x10
        assert_eq!(atc.lookup(0x10, false, false), Some(0x1000));
        atc.flush_page(0x10);
        assert_eq!(atc.lookup(0x10, false, false), None);
    }

    #[test]
    fn permission_violation_does_not_hit() {
        let mut atc = Atc::default();
        // Write-protected, supervisor-only page.
        atc.insert(0x10, true, 0x5000, true, true);
        // A supervisor read hits; a write misses (write-protected) so the caller
        // re-walks and faults.
        assert_eq!(atc.lookup(0x10, true, false), Some(0x5000));
        assert_eq!(atc.lookup(0x10, true, true), None);
    }

    const SHIFT: u32 = 12;

    #[test]
    fn atc_030_entries_are_tagged_by_function_code() {
        let mut atc = Atc030::default();
        assert_eq!(atc.lookup(5, 0x10, SHIFT, false), None);
        atc.insert(5, 0x10, SHIFT, 0x0008_0000, false);
        assert_eq!(atc.lookup(5, 0x10, SHIFT, false), Some(0x0008_0000));
        // Supervisor program space, and user data space, are other entries.
        assert_eq!(atc.lookup(6, 0x10, SHIFT, false), None);
        assert_eq!(atc.lookup(1, 0x10, SHIFT, false), None);
        // A different page size makes every entry stale.
        assert_eq!(atc.lookup(5, 0x10, SHIFT + 1, false), None);
    }

    #[test]
    fn atc_030_write_protected_entry_misses_only_for_writes() {
        let mut atc = Atc030::default();
        atc.insert(1, 0x20, SHIFT, 0x0010_0000, true);
        assert_eq!(atc.lookup(1, 0x20, SHIFT, false), Some(0x0010_0000));
        assert_eq!(atc.lookup(1, 0x20, SHIFT, true), None);
    }

    #[test]
    fn atc_030_holds_22_entries_and_replaces_the_least_recently_used() {
        let mut atc = Atc030::default();
        for page in 0..ATC_030_ENTRIES as u32 {
            atc.insert(1, page, SHIFT, page << SHIFT, false);
        }
        assert_eq!(atc.valid_entries(), ATC_030_ENTRIES);
        // Touch every page but 3, so 3 is now the least recently used.
        for page in (0..ATC_030_ENTRIES as u32).filter(|&p| p != 3) {
            assert!(atc.lookup(1, page, SHIFT, false).is_some());
        }
        atc.insert(1, 100, SHIFT, 0x0064_0000, false);
        assert_eq!(atc.valid_entries(), ATC_030_ENTRIES);
        assert_eq!(atc.lookup(1, 3, SHIFT, false), None);
        assert_eq!(atc.lookup(1, 100, SHIFT, false), Some(0x0064_0000));
        assert!(atc.lookup(1, 4, SHIFT, false).is_some());
        // Re-inserting a cached page updates it in place.
        atc.insert(1, 4, SHIFT, 0x0099_0000, true);
        assert_eq!(atc.valid_entries(), ATC_030_ENTRIES);
        assert_eq!(atc.lookup(1, 4, SHIFT, false), Some(0x0099_0000));
    }

    #[test]
    fn atc_030_probe_answers_like_lookup_without_recording_a_use() {
        let mut atc = Atc030::default();
        for page in 0..ATC_030_ENTRIES as u32 {
            atc.insert(1, page, SHIFT, page << SHIFT, page == 7);
        }
        let before = atc.clone();
        for page in 0..ATC_030_ENTRIES as u32 + 2 {
            for write in [false, true] {
                assert_eq!(
                    atc.probe(1, page, SHIFT, write),
                    atc.clone().lookup(1, page, SHIFT, write),
                    "page {page}, write {write}"
                );
            }
        }
        assert_eq!(atc.probe(2, 3, SHIFT, false), None, "other function code");
        assert_eq!(atc.probe(1, 3, SHIFT + 1, false), None, "other page size");
        assert_eq!(atc, before, "a probe changes nothing");
        // Page 0 is the least recently used; probing it must not save it.
        atc.insert(1, 100, SHIFT, 0x0064_0000, false);
        assert_eq!(atc.probe(1, 0, SHIFT, false), None);
    }

    #[test]
    fn atc_030_hint_never_changes_what_a_lookup_finds() {
        // Pages 0 and 32 share a hint slot, so their hints keep replacing
        // each other; every lookup must still find its own entry.
        let mut atc = Atc030::default();
        atc.insert(5, 0, SHIFT, 0x0010_0000, false);
        atc.insert(5, 32, SHIFT, 0x0020_0000, false);
        atc.insert(6, 1, SHIFT, 0x0030_0000, false);
        for _ in 0..3 {
            assert_eq!(atc.lookup(5, 0, SHIFT, false), Some(0x0010_0000));
            assert_eq!(atc.lookup(6, 1, SHIFT, false), Some(0x0030_0000));
            assert_eq!(atc.lookup(5, 32, SHIFT, false), Some(0x0020_0000));
            assert_eq!(atc.probe(5, 0, SHIFT, false), Some(0x0010_0000));
        }
        // A flushed entry is not found through a hint that still names it.
        atc.flush_fc_page(5, 7, 32 << SHIFT);
        assert_eq!(atc.lookup(5, 32, SHIFT, false), None);
        assert_eq!(atc.probe(5, 32, SHIFT, false), None);
    }

    #[test]
    fn atc_030_pflush_honours_the_function_code_mask_and_page() {
        let mut atc = Atc030::default();
        for fc in [1u8, 2, 5, 6] {
            atc.insert(fc, 0x30, SHIFT, 0x0030_0000, false);
            atc.insert(fc, 0x31, SHIFT, 0x0031_0000, false);
        }
        // PFLUSH #5,#7,(a0) on page 0x30: only supervisor data, that page.
        atc.flush_fc_page(5, 7, 0x30 << SHIFT | 0x123);
        assert_eq!(atc.lookup(5, 0x30, SHIFT, false), None);
        assert!(atc.lookup(5, 0x31, SHIFT, false).is_some());
        assert!(atc.lookup(1, 0x30, SHIFT, false).is_some());
        // PFLUSH #4,#4: every supervisor entry, user entries kept.
        atc.flush_fc(4, 4);
        assert_eq!(atc.lookup(5, 0x31, SHIFT, false), None);
        assert_eq!(atc.lookup(6, 0x30, SHIFT, false), None);
        assert!(atc.lookup(1, 0x31, SHIFT, false).is_some());
        assert!(atc.lookup(2, 0x30, SHIFT, false).is_some());
        // Mask 0 matches every function code.
        atc.flush_fc(0, 0);
        assert_eq!(atc.valid_entries(), 0);
    }
}
