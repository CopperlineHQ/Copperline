// SPDX-License-Identifier: GPL-3.0-or-later

//! Expansion-board state uses explicit IDs, independent of enabled features.

use serde::de::{Error, SeqAccess, Visitor};
use serde::ser::SerializeTuple;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// Keep the enum and its wire IDs together. Retired IDs remain reserved; an
// unavailable feature must never change the interpretation of another board.
macro_rules! board_types {
    ($($(#[$gate:meta])* $variant:ident($ty:ty) = $id:literal,)+) => {
        /// A functional expansion board owned and serialized with the Bus.
        ///
        /// The wire representation is a tuple of a stable u32 kind and that
        /// board's payload. A build without a board's feature rejects its ID.
        #[allow(clippy::large_enum_variant)]
        pub enum BoardDevice {
            $($(#[$gate])* $variant($ty),)+
        }

        impl Serialize for BoardDevice {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut tuple = serializer.serialize_tuple(2)?;
                match self {
                    $($(#[$gate])* Self::$variant(board) => {
                        tuple.serialize_element(&($id as u32))?;
                        tuple.serialize_element(board)?;
                    },)+
                }
                tuple.end()
            }
        }

        impl<'de> Deserialize<'de> for BoardDevice {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct BoardVisitor;
                impl<'de> Visitor<'de> for BoardVisitor {
                    type Value = BoardDevice;

                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("an expansion-board kind and payload")
                    }

                    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                        let id: u32 = seq.next_element()?.ok_or_else(|| A::Error::invalid_length(0, &self))?;
                        match id {
                            $($(#[$gate])* $id => {
                                let board = seq.next_element()?.ok_or_else(|| A::Error::invalid_length(1, &self))?;
                                Ok(BoardDevice::$variant(board))
                            },)+
                            id => {
                                // This match deliberately keeps disabled kinds visible.
                                let name = match id {
                                    $($id => stringify!($variant),)+
                                    _ => return Err(A::Error::custom(format!("unknown expansion-board kind {id}"))),
                                };
                                Err(A::Error::custom(format!("expansion board {name} is not supported by this build")))
                            }
                        }
                    }
                }
                deserializer.deserialize_tuple(2, BoardVisitor)
            }
        }
    };
}

board_types! {
    A2091(crate::a2091::A2091) = 0,
    A4091(crate::a4091::A4091) = 1,
    A2065(crate::a2065::A2065) = 2,
    #[cfg(feature = "wasm-boards")]
    Wasm(crate::wasmboard::WasmBoard) = 3,
    Filesys(crate::filesys::FilesysBoard) = 4,
    Z3660(crate::z3660::Z3660) = 5,
    Picasso2(Box<crate::picasso2::Picasso2>) = 6,
    IdeZorro(crate::ide_zorro::IdeZorro) = 7,
    GraffityZ2(Box<crate::graffity::GraffityZ2>) = 8,
    GraffityZ3(Box<crate::graffity::GraffityZ3>) = 9,
    Toccata(Box<crate::toccata::Toccata>) = 10,
    #[cfg(feature = "mhi")]
    Mhi(Box<crate::mhi::Mhi>) = 11,
    #[cfg(feature = "cd32-fmv")]
    Cd32Fmv(Box<crate::cd32_fmv::Cd32Fmv>) = 12,
    Copperhf(crate::copperhf::CopperhfBoard) = 13,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zorro_device::ZorroDevice;

    // The same fixture is read and reproduced in the full and core-only CI
    // builds. It contains no optional board, so those features must not affect it.
    const COPPERHF: &[u8] = include_bytes!("../../tests/fixtures/copperhf-board-v79.bin");

    #[test]
    fn board_fixture_is_independent_of_optional_features() {
        let board: BoardDevice = bincode::deserialize(COPPERHF).unwrap();
        assert_eq!(board.kind(), "copperhf");
        assert_eq!(bincode::serialize(&board).unwrap(), COPPERHF);
        let fresh = BoardDevice::Copperhf(crate::copperhf::CopperhfBoard::new());
        assert_eq!(bincode::serialize(&fresh).unwrap(), COPPERHF);
    }

    #[test]
    fn unsupported_board_ids_report_the_missing_kind() {
        for (enabled, id, name) in [
            (cfg!(feature = "wasm-boards"), 3u32, "Wasm"),
            (cfg!(feature = "mhi"), 11, "Mhi"),
            (cfg!(feature = "cd32-fmv"), 12, "Cd32Fmv"),
        ] {
            if !enabled {
                let error = bincode::deserialize::<BoardDevice>(&id.to_le_bytes())
                    .err()
                    .unwrap();
                assert!(error
                    .to_string()
                    .contains(&format!("{name} is not supported by this build")));
            }
        }
    }
}
