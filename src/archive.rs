// SPDX-License-Identifier: GPL-3.0-or-later

//! Resource budgets shared by LHA and ZIP package readers.

use std::io::{Read, Write};

use anyhow::{bail, Result};

/// Limits on decoded output and the number of archive headers visited.
/// Sizes are bytes, independent of the compression ratio. Smaller budgets
/// can be used when only a slave is wanted or when exercising the parsers.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub member_bytes: u64,
    pub total_bytes: u64,
    pub entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            member_bytes: 256 * 1024 * 1024,
            total_bytes: 1024 * 1024 * 1024,
            entries: 16_384,
        }
    }
}

impl Limits {
    /// A library scan needs a small WHDLoad slave, never a game's data.
    pub const SLAVE: Self = Self {
        member_bytes: 1024 * 1024,
        total_bytes: 1024 * 1024,
        entries: 16_384,
    };

    pub(crate) fn check_entries(self, count: usize) -> Result<()> {
        if count > self.entries {
            bail!("archive exceeds the {} entry limit", self.entries);
        }
        Ok(())
    }
}

pub(crate) struct Budget {
    limits: Limits,
    entries: usize,
    bytes: u64,
}

impl Budget {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            limits,
            entries: 0,
            bytes: 0,
        }
    }

    pub(crate) fn entry(&mut self) -> Result<()> {
        if self.entries >= self.limits.entries {
            bail!("archive exceeds the {} entry limit", self.limits.entries);
        }
        self.entries += 1;
        Ok(())
    }

    /// Reject advertised oversize members before allocating or creating a
    /// file. The actual stream is bounded separately: headers are untrusted.
    pub(crate) fn allowance(&self, declared: u64) -> Result<u64> {
        if declared > self.limits.member_bytes {
            bail!(
                "archive member exceeds the {} byte limit",
                self.limits.member_bytes
            );
        }
        let remaining = self.limits.total_bytes - self.bytes;
        if declared > remaining {
            bail!(
                "archive exceeds the {} byte total limit",
                self.limits.total_bytes
            );
        }
        Ok(remaining.min(self.limits.member_bytes))
    }

    /// Read to EOF (including the decoder's final CRC check), probing at
    /// most one byte past the budget. Never write that extra byte to disk
    /// or retain it in a returned buffer.
    pub(crate) fn copy(
        &mut self,
        reader: &mut impl Read,
        out: &mut impl Write,
        declared: u64,
    ) -> Result<u64> {
        let limit = self.allowance(declared)?;
        let mut copied = 0;
        let mut buf = [0; 32 * 1024];
        loop {
            let remaining = limit - copied;
            let len = remaining.saturating_add(1).min(buf.len() as u64) as usize;
            let n = match reader.read(&mut buf[..len]) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                other => other?,
            };
            if n == 0 {
                self.bytes += copied;
                return Ok(copied);
            }
            if n as u64 > remaining {
                bail!("archive decompressed output exceeds its {limit} byte allowance");
            }
            out.write_all(&buf[..n])?;
            copied += n as u64;
        }
    }

    pub(crate) fn read(&mut self, reader: &mut impl Read, declared: u64) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        self.copy(reader, &mut data, declared)?;
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: Limits = Limits {
        member_bytes: 4,
        total_bytes: 6,
        entries: 2,
    };

    #[test]
    fn exact_limits_and_empty_members_are_allowed() {
        let mut budget = Budget::new(SMALL);
        budget.entry().unwrap();
        assert_eq!(budget.read(&mut &b"1234"[..], 4).unwrap(), b"1234");
        budget.entry().unwrap();
        assert_eq!(budget.read(&mut &b"56"[..], 2).unwrap(), b"56");
        assert!(budget.read(&mut &b""[..], 0).unwrap().is_empty());
        assert!(budget.entry().is_err());
    }

    #[test]
    fn declared_sizes_are_checked_before_reading() {
        struct MustNotRead;
        impl Read for MustNotRead {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                panic!("oversize member was read");
            }
        }
        assert!(Budget::new(SMALL).read(&mut MustNotRead, 5).is_err());
        let mut budget = Budget::new(SMALL);
        budget.read(&mut &b"1234"[..], 4).unwrap();
        assert!(budget.read(&mut MustNotRead, 3).is_err());
    }

    #[test]
    fn a_lying_size_cannot_overrun_memory_or_disk_budgets() {
        let mut budget = Budget::new(SMALL);
        let mut source = &b"123456789"[..];
        let mut out = Vec::new();
        assert!(budget.copy(&mut source, &mut out, 0).is_err());
        assert!(out.len() <= 4);
        assert_eq!(source, b"6789"); // only one byte beyond the cap was read

        let mut budget = Budget::new(SMALL);
        budget.read(&mut &b"1234"[..], 4).unwrap();
        let mut out = Vec::new();
        assert!(budget.copy(&mut &b"567"[..], &mut out, 0).is_err());
        assert!(out.len() <= 2);
    }
}
