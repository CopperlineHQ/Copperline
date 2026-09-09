// SPDX-License-Identifier: GPL-3.0-or-later

//! Guest code coverage: a per-instruction hit histogram over the program's
//! code hunks, and the `coverage.bin` sidecar that carries it offline.
//!
//! The collector is the lightweight sibling of the precise instruction
//! sampler (`samples.rs`): where a sample records a call stack, cost and
//! registers for every retired instruction, coverage keeps one saturating
//! counter per instruction word inside the supplied code ranges and a single
//! total for everything outside them (Kickstart, libraries, other programs).
//! Nothing is written per frame, and the host cost per instruction is one
//! range check plus an increment, so a coverage run of a whole program is
//! cheap enough to leave on from `LoadSeg()` to exit.
//!
//! Like every other CPU observer it lives beside the machine, never inside
//! a save state, and forces the precise per-instruction loop while armed
//! (a JIT-configured CPU logs its fallback once). It never alters the
//! emulated timeline: the counters observe retired instructions and feed
//! nothing back into the machine.
//!
//! Without code ranges the collector counts every address it sees in a
//! bounded sparse map, so a capture started without relocation data still
//! yields a histogram the offline converter can relocate through
//! `relocation_bases`.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};

/// Distinct addresses kept when no code ranges bound the collector.
pub const SPARSE_ADDRESS_CAP: usize = 1 << 20;

/// `coverage.bin` magic and version.
const MAGIC: &[u8; 4] = b"CLCV";
const VERSION: u32 = 1;

/// One executable range and its per-word hit counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageRange {
    pub base: u32,
    pub size: u32,
    /// One saturating counter per 2-byte word of the range.
    pub counts: Vec<u32>,
}

impl CoverageRange {
    fn new(base: u32, size: u32) -> Self {
        Self {
            base,
            size,
            counts: vec![0; (size as usize).div_ceil(2)],
        }
    }

    fn slot(&self, pc: u32) -> Option<usize> {
        let offset = pc.wrapping_sub(self.base);
        (pc >= self.base && offset < self.size).then_some((offset >> 1) as usize)
    }
}

/// The finished histogram: what the live collector accumulates and what
/// `coverage.bin` stores.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverageData {
    pub ranges: Vec<CoverageRange>,
    /// Addresses counted outside any range, present only when the
    /// collector ran without ranges.
    pub sparse: Vec<(u32, u32)>,
    /// Retired instructions whose PC fell outside every range (or past the
    /// sparse cap): ROM, libraries, other tasks.
    pub outside_hits: u64,
    /// Every retired instruction the collector saw.
    pub total_hits: u64,
}

impl CoverageData {
    /// Every `(runtime address, count)` pair with a nonzero count.
    pub fn hits(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.ranges
            .iter()
            .flat_map(|range| {
                range
                    .counts
                    .iter()
                    .enumerate()
                    .filter(|(_, count)| **count != 0)
                    .map(move |(index, count)| (range.base + (index as u32) * 2, *count))
            })
            .chain(self.sparse.iter().copied())
    }

    /// Instructions retired inside the ranges (or the sparse map).
    pub fn program_hits(&self) -> u64 {
        self.total_hits.saturating_sub(self.outside_hits)
    }

    /// Fold another histogram over the same ranges into this one (a second
    /// capture of the same load), saturating.
    pub fn merge(&mut self, other: &CoverageData) {
        for range in &other.ranges {
            match self
                .ranges
                .iter_mut()
                .find(|mine| mine.base == range.base && mine.size == range.size)
            {
                Some(mine) => {
                    for (a, b) in mine.counts.iter_mut().zip(&range.counts) {
                        *a = a.saturating_add(*b);
                    }
                }
                None => self.ranges.push(range.clone()),
            }
        }
        let mut sparse: BTreeMap<u32, u32> = self.sparse.iter().copied().collect();
        for &(addr, count) in &other.sparse {
            let slot = sparse.entry(addr).or_default();
            *slot = slot.saturating_add(count);
        }
        self.sparse = sparse.into_iter().collect();
        self.outside_hits = self.outside_hits.saturating_add(other.outside_hits);
        self.total_hits = self.total_hits.saturating_add(other.total_hits);
    }

    /// Serialize as `coverage.bin`: little-endian throughout.
    pub fn write_to(&self, mut out: impl Write) -> io::Result<()> {
        out.write_all(MAGIC)?;
        out.write_all(&VERSION.to_le_bytes())?;
        out.write_all(&(self.ranges.len() as u32).to_le_bytes())?;
        for range in &self.ranges {
            out.write_all(&range.base.to_le_bytes())?;
            out.write_all(&range.size.to_le_bytes())?;
            out.write_all(&(range.counts.len() as u32).to_le_bytes())?;
            for count in &range.counts {
                out.write_all(&count.to_le_bytes())?;
            }
        }
        out.write_all(&(self.sparse.len() as u32).to_le_bytes())?;
        for (addr, count) in &self.sparse {
            out.write_all(&addr.to_le_bytes())?;
            out.write_all(&count.to_le_bytes())?;
        }
        out.write_all(&self.outside_hits.to_le_bytes())?;
        out.write_all(&self.total_hits.to_le_bytes())?;
        Ok(())
    }

    pub fn read_from(mut input: impl Read) -> io::Result<Self> {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes)?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
        if bytes.len() < 12 || &bytes[..4] != MAGIC {
            return Err(bad("not a Copperline coverage file (CLCV)"));
        }
        let mut cursor = 4usize;
        let u32_at = |cursor: &mut usize| -> io::Result<u32> {
            let end = *cursor + 4;
            let word = bytes
                .get(*cursor..end)
                .ok_or_else(|| bad("coverage file truncated"))?;
            *cursor = end;
            Ok(u32::from_le_bytes(word.try_into().expect("four bytes")))
        };
        let version = u32_at(&mut cursor)?;
        if version != VERSION {
            return Err(bad(&format!("unsupported coverage file version {version}")));
        }
        let range_count = u32_at(&mut cursor)? as usize;
        let mut ranges = Vec::with_capacity(range_count.min(1024));
        for _ in 0..range_count {
            let base = u32_at(&mut cursor)?;
            let size = u32_at(&mut cursor)?;
            let words = u32_at(&mut cursor)? as usize;
            if words != (size as usize).div_ceil(2) {
                return Err(bad("coverage range word count does not match its size"));
            }
            let mut counts = Vec::with_capacity(words);
            for _ in 0..words {
                counts.push(u32_at(&mut cursor)?);
            }
            ranges.push(CoverageRange { base, size, counts });
        }
        let sparse_count = u32_at(&mut cursor)? as usize;
        let mut sparse = Vec::with_capacity(sparse_count.min(SPARSE_ADDRESS_CAP));
        for _ in 0..sparse_count {
            let addr = u32_at(&mut cursor)?;
            let count = u32_at(&mut cursor)?;
            sparse.push((addr, count));
        }
        let u64_at = |cursor: &mut usize| -> io::Result<u64> {
            let end = *cursor + 8;
            let word = bytes
                .get(*cursor..end)
                .ok_or_else(|| bad("coverage file truncated"))?;
            *cursor = end;
            Ok(u64::from_le_bytes(word.try_into().expect("eight bytes")))
        };
        let outside_hits = u64_at(&mut cursor)?;
        let total_hits = u64_at(&mut cursor)?;
        if cursor != bytes.len() {
            return Err(bad("coverage file has trailing bytes"));
        }
        Ok(Self {
            ranges,
            sparse,
            outside_hits,
            total_hits,
        })
    }
}

/// The live per-instruction counter, owned by the CPU while armed.
#[derive(Debug, Clone)]
pub struct CoverageCollector {
    ranges: Vec<CoverageRange>,
    sparse: BTreeMap<u32, u32>,
    outside_hits: u64,
    total_hits: u64,
}

impl CoverageCollector {
    /// `ranges` are `(base, size)` runtime extents of the program's code
    /// hunks; empty means count every address (bounded).
    pub fn new(ranges: &[(u32, u32)]) -> Self {
        Self {
            ranges: ranges
                .iter()
                .filter(|(_, size)| *size != 0)
                .map(|&(base, size)| CoverageRange::new(base, size))
                .collect(),
            sparse: BTreeMap::new(),
            outside_hits: 0,
            total_hits: 0,
        }
    }

    pub fn ranges(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.ranges.iter().map(|range| (range.base, range.size))
    }

    /// One retired instruction at `pc`.
    #[inline]
    pub fn hit(&mut self, pc: u32) {
        self.total_hits = self.total_hits.wrapping_add(1);
        let pc = pc & !1;
        if self.ranges.is_empty() {
            if self.sparse.len() < SPARSE_ADDRESS_CAP || self.sparse.contains_key(&pc) {
                let slot = self.sparse.entry(pc).or_default();
                *slot = slot.saturating_add(1);
            } else {
                self.outside_hits = self.outside_hits.wrapping_add(1);
            }
            return;
        }
        for range in &mut self.ranges {
            if let Some(slot) = range.slot(pc) {
                range.counts[slot] = range.counts[slot].saturating_add(1);
                return;
            }
        }
        self.outside_hits = self.outside_hits.wrapping_add(1);
    }

    pub fn total_hits(&self) -> u64 {
        self.total_hits
    }

    /// A copy of the counters so far (periodic flushes while collecting).
    pub fn snapshot(&self) -> CoverageData {
        CoverageData {
            ranges: self.ranges.clone(),
            sparse: self.sparse.iter().map(|(a, c)| (*a, *c)).collect(),
            outside_hits: self.outside_hits,
            total_hits: self.total_hits,
        }
    }

    pub fn into_data(self) -> CoverageData {
        CoverageData {
            ranges: self.ranges,
            sparse: self.sparse.into_iter().collect(),
            outside_hits: self.outside_hits,
            total_hits: self.total_hits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_words_inside_ranges_and_totals_the_rest() {
        let mut collector = CoverageCollector::new(&[(0x1000, 0x10), (0x3000, 0x4)]);
        collector.hit(0x1000);
        collector.hit(0x1001); // odd PCs land on their word
        collector.hit(0x100e);
        collector.hit(0x1010); // one past the range
        collector.hit(0x3002);
        collector.hit(0xf8_0000);
        let data = collector.into_data();
        assert_eq!(data.total_hits, 6);
        assert_eq!(data.outside_hits, 2);
        assert_eq!(data.program_hits(), 4);
        let hits: Vec<(u32, u32)> = data.hits().collect();
        assert_eq!(hits, vec![(0x1000, 2), (0x100e, 1), (0x3002, 1)]);
    }

    #[test]
    fn saturates_instead_of_wrapping() {
        let mut collector = CoverageCollector::new(&[(0x1000, 2)]);
        collector.ranges[0].counts[0] = u32::MAX - 1;
        collector.hit(0x1000);
        collector.hit(0x1000);
        assert_eq!(collector.snapshot().ranges[0].counts[0], u32::MAX);
    }

    #[test]
    fn without_ranges_every_address_is_kept_sparsely() {
        let mut collector = CoverageCollector::new(&[]);
        collector.hit(0x2000);
        collector.hit(0x2000);
        collector.hit(0xf8_0010);
        let data = collector.into_data();
        assert_eq!(data.sparse, vec![(0x2000, 2), (0xf8_0010, 1)]);
        assert_eq!(data.outside_hits, 0);
        assert_eq!(data.hits().count(), 2);
    }

    #[test]
    fn binary_sidecar_round_trips_and_rejects_garbage() {
        let mut collector = CoverageCollector::new(&[(0x1000, 0x6)]);
        collector.hit(0x1002);
        collector.hit(0x9000);
        let data = collector.into_data();
        let mut bytes = Vec::new();
        data.write_to(&mut bytes).unwrap();
        assert_eq!(&bytes[..4], b"CLCV");
        assert_eq!(CoverageData::parse(&bytes).unwrap(), data);
        assert!(CoverageData::parse(&bytes[..bytes.len() - 1]).is_err());
        assert!(CoverageData::parse(b"nope").is_err());
        let mut sparse = CoverageCollector::new(&[]);
        sparse.hit(0x10);
        let sparse = sparse.into_data();
        let mut bytes = Vec::new();
        sparse.write_to(&mut bytes).unwrap();
        assert_eq!(CoverageData::parse(&bytes).unwrap(), sparse);
    }

    /// The precise loop's retire hook: the control test machine spins in a
    /// four-instruction ROM loop at $F80010, so a frame of execution counts
    /// every one of those words and nothing else.
    #[cfg(feature = "control")]
    #[test]
    fn retired_instructions_are_counted_by_the_precise_loop() {
        let mut emu = crate::control::test_emulator();
        assert!(emu.machine.start_coverage(&[(0x00F8_0010, 0x10)]));
        assert!(!emu.machine.start_coverage(&[]), "one collector at a time");
        emu.step_frame().expect("frame");
        let data = emu.machine.stop_coverage().expect("armed").into_data();
        assert!(emu.machine.stop_coverage().is_none());
        let hits: Vec<(u32, u32)> = data.hits().collect();
        let addresses: Vec<u32> = hits.iter().map(|(addr, _)| *addr).collect();
        assert_eq!(
            addresses,
            vec![0x00F8_0010, 0x00F8_0012, 0x00F8_0014, 0x00F8_001A],
            "{hits:?}"
        );
        let counts: Vec<u32> = hits.iter().map(|(_, count)| *count).collect();
        assert!(counts.iter().all(|c| *c > 100), "{hits:?}");
        assert!(
            counts.windows(2).all(|w| w[0].abs_diff(w[1]) <= 1),
            "{hits:?}"
        );
        assert_eq!(data.total_hits, u64::from(counts.iter().sum::<u32>()));
        assert_eq!(data.outside_hits, 0);
    }

    #[test]
    fn merge_adds_matching_ranges_and_sparse_entries() {
        let mut first = CoverageCollector::new(&[(0x1000, 4)]);
        first.hit(0x1000);
        let mut second = CoverageCollector::new(&[(0x1000, 4), (0x2000, 2)]);
        second.hit(0x1000);
        second.hit(0x1002);
        second.hit(0x2000);
        let mut merged = first.into_data();
        merged.merge(&second.into_data());
        assert_eq!(merged.ranges[0].counts, vec![2, 1]);
        assert_eq!(merged.ranges[1].counts, vec![1]);
        assert_eq!(merged.total_hits, 4);
    }
}
