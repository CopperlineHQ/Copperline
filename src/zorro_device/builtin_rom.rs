// SPDX-License-Identifier: GPL-3.0-or-later

//! Board firmware compiled into Copperline itself.
//!
//! Most of the ROM a machine maps either comes from a file the user names
//! (Kickstart, an A2091/A4091/LIDE image, an HRTMon cartridge) or travels in
//! a save state with the board that serves it: the board keeps the bytes in
//! a serialized field (the A2091's `rom`, the services board's window
//! image, the uaelib trap page). The firmware listed here does neither. The
//! board serves bytes embedded in the binary, so a save state carries only
//! the board's registers and the guest memory that points into that code,
//! and a load supplies whatever bytes the loading build was compiled with.
//! A state is only meaningful against the firmware it was saved with, so
//! `savestate::board_rom` records the identity of every entry a machine
//! maps and refuses a state whose firmware differs from this build's.

use std::sync::OnceLock;

/// One piece of board firmware embedded in the binary (see the module
/// documentation for what qualifies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinRom {
    /// `copperhf.device` and its INT2 server, which run in place from the
    /// copperhf board's window (`[copperhf]`).
    Copperhf,
    /// The HostSocket board's `bsdsocket.library` autoboot ROM, served to
    /// the guest by the board's plugin (`[hostsocket]`).
    #[cfg(feature = "wasm-boards")]
    HostSocketRom,
    /// The HostSocket board's plugin module. A state replays the module's
    /// linear memory, which only the same module can interpret.
    #[cfg(feature = "wasm-boards")]
    HostSocketPlugin,
    /// The zz9k crypto board's plugin module (`[zz9k]`), likewise replayed.
    #[cfg(feature = "wasm-boards")]
    Zz9kPlugin,
}

impl BuiltinRom {
    /// Every entry this build embeds, in a fixed order.
    pub const ALL: &'static [BuiltinRom] = &[
        BuiltinRom::Copperhf,
        #[cfg(feature = "wasm-boards")]
        BuiltinRom::HostSocketRom,
        #[cfg(feature = "wasm-boards")]
        BuiltinRom::HostSocketPlugin,
        #[cfg(feature = "wasm-boards")]
        BuiltinRom::Zz9kPlugin,
    ];

    /// The stable name a save state records the entry under. Never rename
    /// or reuse one: a state written by another build is matched by it.
    pub fn id(self) -> &'static str {
        match self {
            BuiltinRom::Copperhf => "copperhf-rom",
            #[cfg(feature = "wasm-boards")]
            BuiltinRom::HostSocketRom => "hostsocket-rom",
            #[cfg(feature = "wasm-boards")]
            BuiltinRom::HostSocketPlugin => "hostsocket-plugin",
            #[cfg(feature = "wasm-boards")]
            BuiltinRom::Zz9kPlugin => "zz9k-plugin",
        }
    }

    /// The entry whose [`id`](Self::id) is `id`, if this build embeds it.
    pub fn from_id(id: &str) -> Option<BuiltinRom> {
        Self::ALL.iter().copied().find(|rom| rom.id() == id)
    }

    /// What the bytes are, for messages.
    pub fn what(self) -> &'static str {
        match self {
            BuiltinRom::Copperhf => "copperhf.device boot ROM",
            #[cfg(feature = "wasm-boards")]
            BuiltinRom::HostSocketRom => "bsdsocket.library boot ROM",
            #[cfg(feature = "wasm-boards")]
            BuiltinRom::HostSocketPlugin => "plugin module",
            #[cfg(feature = "wasm-boards")]
            BuiltinRom::Zz9kPlugin => "plugin module",
        }
    }

    /// The board that maps the bytes, as its configuration section names
    /// it, for messages.
    pub fn board(self) -> &'static str {
        match self {
            BuiltinRom::Copperhf => "copperhf hard-drive board ([copperhf])",
            #[cfg(feature = "wasm-boards")]
            BuiltinRom::HostSocketRom | BuiltinRom::HostSocketPlugin => {
                "HostSocket board ([hostsocket])"
            }
            #[cfg(feature = "wasm-boards")]
            BuiltinRom::Zz9kPlugin => "zz9k crypto board ([zz9k])",
        }
    }

    /// The bytes this build embeds.
    pub fn bytes(self) -> &'static [u8] {
        match self {
            BuiltinRom::Copperhf => crate::copperhf::COPPERHF_ROM,
            #[cfg(feature = "wasm-boards")]
            BuiltinRom::HostSocketRom => crate::hostsocket::HOSTSOCKET_ROM,
            #[cfg(feature = "wasm-boards")]
            BuiltinRom::HostSocketPlugin => crate::hostsocket::HOSTSOCKET_WASM,
            #[cfg(feature = "wasm-boards")]
            BuiltinRom::Zz9kPlugin => crate::zz9k::ZZ9K_WASM,
        }
    }

    /// SHA-256 of [`bytes`](Self::bytes), lowercase hex. Computed once per
    /// process: frontend checkpoints record it every field.
    pub fn sha256(self) -> &'static str {
        static DIGESTS: [OnceLock<String>; BuiltinRom::ALL.len()] =
            [const { OnceLock::new() }; BuiltinRom::ALL.len()];
        let index = Self::ALL
            .iter()
            .position(|&rom| rom == self)
            .expect("every entry is listed in ALL");
        DIGESTS[index].get_or_init(|| crate::hash::sha256_hex(self.bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_round_trip() {
        for (i, rom) in BuiltinRom::ALL.iter().enumerate() {
            assert_eq!(BuiltinRom::from_id(rom.id()), Some(*rom));
            assert!(
                BuiltinRom::ALL[i + 1..]
                    .iter()
                    .all(|other| other.id() != rom.id()),
                "{} is listed twice",
                rom.id()
            );
        }
        assert_eq!(BuiltinRom::from_id("no-such-rom"), None);
    }

    #[test]
    fn digest_is_the_sha256_of_the_embedded_bytes() {
        for &rom in BuiltinRom::ALL {
            assert!(!rom.bytes().is_empty(), "{} is empty", rom.id());
            assert_eq!(rom.sha256(), crate::hash::sha256_hex(rom.bytes()));
            assert_eq!(rom.sha256().len(), 64);
        }
    }
}
