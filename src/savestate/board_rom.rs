// SPDX-License-Identifier: GPL-3.0-or-later

//! The `BROM` chunk: which built-in board firmware the saved machine ran.
//!
//! A board that serves firmware compiled into the binary
//! ([`BuiltinRom`]: the copperhf boot ROM, the bundled plugin boards'
//! modules and ROM) keeps none of those bytes in its serialized state. The
//! guest's driver, already initialised from them, lives on in the state's
//! RAM and the board's registers, and a load maps whatever bytes the
//! loading build embeds. Under different firmware the driver resumes
//! against code that has moved and the guest hangs (a Kickstart 3.1 A1200
//! booted from a copperhf drive under 1.1.0 stops at an empty Workbench
//! screen in 1.2.0, whose copperhf ROM changed).
//!
//! The chunk records the identity (length and SHA-256) of each such piece
//! the machine maps, and a load refuses a state whose firmware differs
//! from this build's with the live machine untouched -- before any board
//! is rebuilt, in the chunk order Copperline writes (a file rewritten with
//! the board chunk first rebuilds its boards before the check, and a board
//! whose restore fails outright reports that instead). It is optional: a state
//! written before the chunk existed carries no identities and loads as it
//! always did, with a warning naming the firmware it could not check.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::bus::Bus;
use crate::zorro_device::BuiltinRom;

/// The identity of one piece of built-in firmware, as a state records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RomRecord {
    /// [`BuiltinRom::id`].
    pub id: String,
    /// Length in bytes.
    pub len: u64,
    /// SHA-256 of the bytes, lowercase hex.
    pub sha256: String,
}

impl RomRecord {
    fn of(rom: BuiltinRom) -> Self {
        Self {
            id: rom.id().to_string(),
            len: rom.bytes().len() as u64,
            sha256: rom.sha256().to_string(),
        }
    }

    /// Whether this record names exactly the bytes `rom` embeds.
    fn matches(&self, rom: BuiltinRom) -> bool {
        self.len == rom.bytes().len() as u64 && self.sha256.eq_ignore_ascii_case(rom.sha256())
    }
}

/// The `BROM` chunk payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BoardRoms {
    /// One record per piece of firmware the machine maps, in board order,
    /// each piece once however many boards map it.
    #[serde(default)]
    pub roms: Vec<RomRecord>,
}

/// The built-in firmware `bus` maps, each piece once, in board order.
fn mapped(bus: &Bus) -> Vec<BuiltinRom> {
    let mut roms = Vec::new();
    for rom in bus.devices.iter().flat_map(|board| board.builtin_roms()) {
        if !roms.contains(&rom) {
            roms.push(rom);
        }
    }
    roms
}

/// The first sixteen hex digits of a digest: enough to tell two apart in
/// a message. Character-wise, since a state's record is untrusted text.
fn short(digest: &str) -> String {
    digest.chars().take(16).collect()
}

impl BoardRoms {
    /// What a state of the machine on `bus` records.
    pub fn of(bus: &Bus) -> Self {
        Self {
            roms: mapped(bus).into_iter().map(RomRecord::of).collect(),
        }
    }

    /// Refuse a state whose recorded firmware differs from this build's.
    /// A record this build has no firmware for (a board it lacks) is only
    /// logged: the board's own chunk decides whether it can load.
    pub fn check(&self) -> Result<()> {
        let mut differences = Vec::new();
        for record in &self.roms {
            let Some(rom) = BuiltinRom::from_id(&record.id) else {
                log::warn!(
                    "save state: records built-in board firmware {:?}, which this build does \
                     not embed, so it cannot be checked",
                    record.id
                );
                continue;
            };
            if !record.matches(rom) {
                differences.push(format!(
                    "the {} of the {} is {} bytes (SHA-256 {}...) in this build but was {} bytes \
                     (SHA-256 {}...) when the state was saved",
                    rom.what(),
                    rom.board(),
                    rom.bytes().len(),
                    short(rom.sha256()),
                    record.len,
                    short(&record.sha256),
                ));
            }
        }
        if differences.is_empty() {
            return Ok(());
        }
        bail!(
            "this save state was made with different built-in board firmware than this build \
             of Copperline: {}. Resuming would run the guest against firmware that has changed \
             under it, which hangs it, so the state was not loaded. Boot the machine afresh \
             instead, or load the state with the Copperline version that saved it",
            differences.join("; ")
        )
    }
}

/// What a load warns about the built-in firmware the restored `bus` maps
/// that `recorded` does not vouch for -- all of it, for a state written
/// before the `BROM` chunk existed. Such a state still loads, as it always
/// did; `None` when there is nothing unchecked.
pub(crate) fn unchecked_warning(bus: &Bus, recorded: Option<&BoardRoms>) -> Option<String> {
    let unchecked: Vec<String> = mapped(bus)
        .into_iter()
        .filter(|rom| {
            !recorded.is_some_and(|recorded| recorded.roms.iter().any(|r| r.id == rom.id()))
        })
        .map(|rom| format!("the {} of the {}", rom.what(), rom.board()))
        .collect();
    (!unchecked.is_empty()).then(|| {
        format!(
            "save state: does not record which built-in board firmware it was saved with (an \
             older Copperline wrote it), so {} cannot be checked against this build. If the \
             guest hangs after this load, that firmware has changed since the state was saved: \
             boot the machine afresh",
            unchecked.join(" and ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(rom: BuiltinRom) -> RomRecord {
        RomRecord::of(rom)
    }

    #[test]
    fn matching_records_pass() {
        let roms = BoardRoms {
            roms: BuiltinRom::ALL.iter().copied().map(record).collect(),
        };
        roms.check().unwrap();
        // The digest compares without regard to case.
        let mut upper = record(BuiltinRom::Copperhf);
        upper.sha256 = upper.sha256.to_ascii_uppercase();
        BoardRoms { roms: vec![upper] }.check().unwrap();
        BoardRoms::default().check().unwrap();
    }

    #[test]
    fn a_different_copperhf_rom_is_refused_naming_the_board() {
        // As a 1.1.0 state would record it: the 5,260-byte ROM.
        let old = RomRecord {
            id: BuiltinRom::Copperhf.id().to_string(),
            len: 5260,
            sha256: "0123456789abcdef".repeat(4),
        };
        let err = BoardRoms { roms: vec![old] }
            .check()
            .unwrap_err()
            .to_string();
        assert!(err.contains("copperhf.device boot ROM"), "{err}");
        assert!(
            err.contains("copperhf hard-drive board ([copperhf])"),
            "{err}"
        );
        assert!(
            err.contains("was 5260 bytes (SHA-256 0123456789abcdef...)"),
            "{err}"
        );
        let here = format!(
            "is {} bytes (SHA-256 {}...) in this build",
            crate::copperhf::COPPERHF_ROM.len(),
            &BuiltinRom::Copperhf.sha256()[..16]
        );
        assert!(err.contains(&here), "{err}");
        assert!(err.contains("Boot the machine afresh"), "{err}");
    }

    #[test]
    fn same_digest_different_length_is_refused() {
        let mut odd = record(BuiltinRom::Copperhf);
        odd.len += 1;
        assert!(BoardRoms { roms: vec![odd] }.check().is_err());
    }

    #[test]
    fn untrusted_digest_text_is_shortened_by_character() {
        let odd = RomRecord {
            id: BuiltinRom::Copperhf.id().to_string(),
            len: 1,
            sha256: "\u{e9}".repeat(40),
        };
        let err = BoardRoms { roms: vec![odd] }
            .check()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&format!("{}...", "\u{e9}".repeat(16))),
            "{err}"
        );
    }

    #[test]
    fn firmware_this_build_lacks_is_not_a_mismatch() {
        let foreign = RomRecord {
            id: "future-board-rom".to_string(),
            len: 4,
            sha256: "00".repeat(32),
        };
        BoardRoms {
            roms: vec![foreign, record(BuiltinRom::Copperhf)],
        }
        .check()
        .unwrap();
    }
}
