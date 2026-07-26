// SPDX-License-Identifier: GPL-3.0-or-later

//! Self-modifying-code detector: notice when a write lands on memory the
//! CPU has already executed.
//!
//! Self-modification is legitimate on a 68000 -- decrunchers, trackers
//! and copper-list patchers all do it -- but it is also where a
//! prefetch-related bug hides, because the 68000 has already fetched the
//! word ahead of the one it is executing. A patch applied too late runs
//! the *old* instruction once; a patch applied to the wrong address
//! corrupts a routine that will not misbehave until it is next called.
//! Neither leaves a trace at the moment it happens.
//!
//! The detector keeps one bit per word of the 24-bit address space
//! marking "the CPU has executed an instruction here", set as each
//! instruction retires, and reports a write that lands on a marked word.
//! Reports are deduplicated by (written address, writing PC) and
//! counted, so a decruncher's inner loop is one entry rather than a
//! flood.
//!
//! Marking at retirement rather than at prefetch is deliberate. The
//! 68000's prefetch queue also reads a word past the end of the
//! instruction stream, which the CPU may never execute; marking that
//! would report a write to the data following a routine as if it were
//! code. The cost is one uncaught case: an instruction that patches its
//! own extension words during its only execution is not reported,
//! because nothing had run there yet. Every repeating pattern -- loops,
//! decrunchers, patch-then-call -- is caught on the pass after the
//! first.
//!
//! Being executed is a property of the address, not of the program: this
//! knows nothing about what is running, only what has run.

use std::collections::BTreeMap;

/// One bit per word of the 24-bit address space Agnus and a 68000 share.
const WORDS: usize = 0x0100_0000 / 2;
const BITMAP_LEN: usize = WORDS / 64;

/// Distinct (address, writer) pairs retained before the report stops
/// growing.
pub const MAX_REPORTS: usize = 256;

/// One place code was written, and by what.
#[derive(Clone, Copy, Debug)]
pub struct SmcReport {
    /// The word written over.
    pub addr: u32,
    /// The instruction that wrote it.
    pub writer_pc: u32,
    /// How far ahead of the write the CPU was executing. A patch landing
    /// within a few bytes of the PC is the prefetch-sensitive case: the
    /// 68000 has already fetched past it.
    pub distance: i64,
    pub count: u64,
}

pub struct SmcTracker {
    /// Set for every word an instruction has retired from.
    executed: Vec<u64>,
    reports: BTreeMap<(u32, u32), SmcReport>,
    pub dropped: u64,
}

impl Default for SmcTracker {
    fn default() -> Self {
        Self {
            executed: vec![0; BITMAP_LEN],
            reports: BTreeMap::new(),
            dropped: 0,
        }
    }
}

impl SmcTracker {
    /// Mark `len` bytes at `addr` as executed code.
    pub fn mark_executed(&mut self, addr: u32, len: u32) {
        for word in words_of(addr, len) {
            self.executed[word / 64] |= 1 << (word % 64);
        }
    }

    pub fn is_executed(&self, addr: u32) -> bool {
        let word = (addr as usize & 0x00FF_FFFF) / 2;
        self.executed[word / 64] & (1 << (word % 64)) != 0
    }

    /// Record a write of `len` bytes at `addr` by the instruction at
    /// `pc`. Returns the report when this is the first time that
    /// (address, writer) pair has been seen, for one-shot logging.
    pub fn note_write(&mut self, addr: u32, len: u32, pc: u32) -> Option<SmcReport> {
        let mut first = None;
        for word in words_of(addr, len) {
            if self.executed[word / 64] & (1 << (word % 64)) == 0 {
                continue;
            }
            let hit = (word * 2) as u32;
            let key = (hit, pc);
            if let Some(entry) = self.reports.get_mut(&key) {
                entry.count += 1;
                continue;
            }
            if self.reports.len() >= MAX_REPORTS {
                self.dropped += 1;
                continue;
            }
            let report = SmcReport {
                addr: hit,
                writer_pc: pc,
                distance: i64::from(hit) - i64::from(pc),
                count: 1,
            };
            self.reports.insert(key, report);
            first.get_or_insert(report);
        }
        first
    }

    /// Every report, most-repeated first.
    pub fn reports(&self) -> Vec<SmcReport> {
        let mut out: Vec<SmcReport> = self.reports.values().copied().collect();
        out.sort_by(|a, b| b.count.cmp(&a.count).then(a.addr.cmp(&b.addr)));
        out
    }

    pub fn clear_reports(&mut self) {
        self.reports.clear();
        self.dropped = 0;
    }

    pub fn describe(report: &SmcReport) -> String {
        format!(
            "code at ${:06X} written by ${:06X}{}{}",
            report.addr,
            report.writer_pc,
            // A 68000 has already prefetched the word after the one it is
            // executing, so a patch this close may be too late to take
            // effect on this pass.
            match report.distance {
                d if (0..=8).contains(&d) => format!(" ({d} bytes ahead, inside the prefetch)"),
                d if d > 0 => format!(" ({d} bytes ahead)"),
                d => format!(" ({} bytes behind)", -d),
            },
            match report.count {
                1 => String::new(),
                n => format!(", {n} times"),
            }
        )
    }
}

/// The word indices `len` bytes at `addr` touch, inside the 24-bit space.
fn words_of(addr: u32, len: u32) -> impl Iterator<Item = usize> {
    let start = (addr & 0x00FF_FFFF) as usize / 2;
    let end = ((addr.wrapping_add(len.max(1)).wrapping_sub(1)) & 0x00FF_FFFF) as usize / 2;
    // A transfer that wraps the 24-bit space is reported at its start
    // word only, rather than sweeping the whole bitmap.
    let end = if end < start { start } else { end };
    (start..=end).take(8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_over_unexecuted_memory_is_not_a_finding() {
        let mut smc = SmcTracker::default();
        assert!(smc.note_write(0x20000, 2, 0xF80010).is_none());
        assert!(smc.reports().is_empty());
    }

    #[test]
    fn a_write_over_executed_code_is_reported_once_then_counted() {
        let mut smc = SmcTracker::default();
        smc.mark_executed(0x20000, 2);
        let first = smc
            .note_write(0x20000, 2, 0x20100)
            .expect("first hit reported");
        assert_eq!(first.addr, 0x20000);
        assert_eq!(first.writer_pc, 0x20100);
        // The same pair again only bumps the tally.
        assert!(smc.note_write(0x20000, 2, 0x20100).is_none());
        let reports = smc.reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].count, 2);
    }

    #[test]
    fn a_long_write_covers_every_word_it_spans() {
        let mut smc = SmcTracker::default();
        smc.mark_executed(0x20002, 2);
        // A longword write at $20000 covers $20000 and $20002.
        assert!(smc.note_write(0x20000, 4, 0x30000).is_some());
        assert_eq!(smc.reports()[0].addr, 0x20002);
    }

    #[test]
    fn the_distance_to_the_writing_pc_flags_the_prefetch_window() {
        let mut smc = SmcTracker::default();
        smc.mark_executed(0x20004, 2);
        smc.note_write(0x20004, 2, 0x20000);
        let line = SmcTracker::describe(&smc.reports()[0]);
        assert!(line.contains("inside the prefetch"), "{line}");

        let mut smc = SmcTracker::default();
        smc.mark_executed(0x30000, 2);
        smc.note_write(0x30000, 2, 0x20000);
        let line = SmcTracker::describe(&smc.reports()[0]);
        assert!(line.contains("65536 bytes ahead"), "{line}");
        assert!(!line.contains("prefetch"), "{line}");
    }

    #[test]
    fn the_report_is_bounded_and_counts_what_it_dropped() {
        let mut smc = SmcTracker::default();
        smc.mark_executed(0x20000, 2);
        for pc in 0..(MAX_REPORTS as u32 + 5) {
            smc.note_write(0x20000, 2, pc * 2);
        }
        assert_eq!(smc.reports().len(), MAX_REPORTS);
        assert_eq!(smc.dropped, 5);
    }
}
