// SPDX-License-Identifier: GPL-3.0-or-later

//! MacroSystem Toccata: a Zorro II AD1848 sound board with a stock,
//! open-source AHI driver (`toccata.audio`), so AHI-aware Amiga software
//! gets 16-bit sound with no board-specific driver work of Copperline's
//! own. See `docs/internals/toccata.md`.
//!
//! `ad1848` is the register-accurate codec/FIFO core, modelled against
//! WinUAE/amiberry's `sndboard.cpp` as a behavioural oracle. The board
//! wrapper (autoconfig, address decode, the Zorro/mixer integration) lands
//! alongside it in the same milestone.

pub mod ad1848;
